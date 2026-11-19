//! Sistema de objetos en disco sobre el servicio de bloques (T1.3).
//!
//! Diseño deliberadamente pequeño y auditable:
//!
//! - Los sectores 0 y 1 son superblocks duplicados (A y B). Cada uno guarda
//!   `generation`, `root_lba`, `root_hash`, `next_free` y un SHA-256 sobre los
//!   bytes `0..480` almacenado en los últimos 32 bytes del sector. `format`
//!   escribe ambos con generación 1; `commit` incrementa la generación y
//!   escribe solo el slot `generation % 2`, dejando el otro como fallback.
//!   `open` valida ambos (magia, versión, slot, checksum y root) y elige el de
//!   mayor generación.
//! - Los nodos ocupan un sector de 512 bytes y llevan su propio SHA-256 en los
//!   últimos 32 bytes (también sobre `0..480`). `root_hash` y los `child_sha256`
//!   son SHA-256 del sector completo. Tipos: hoja y nodo interno.
//! - Entrada de hoja: `key u64, lba u64, len u32, reserved u32,
//!   data_sha256[32]` (56 bytes). Entrada interna: `min_key u64, child_lba u64,
//!   child_sha256[32]` (48 bytes).
//! - `FANOUT = 8`: como máximo 8 entradas por nodo. El árbol usa una raíz
//!   interna y hasta 8 hojas, así que admite `8 * 8 = 64` objetos. Superar ese
//!   límite devuelve [`ObjError::TooManyObjects`] (suficiente para la alpha;
//!   niveles adicionales se dejan para el sistema de objetos definitivo).
//! - Los valores son extents contiguos de 1..8 sectores (hasta 4096 bytes) con
//!   SHA-256 calculado solo sobre los bytes útiles; `read` lo verifica.
//! - Todas las escrituras son *copy-on-write*: se escriben el valor, las hojas,
//!   los nodos internos y la raíz en sectores nuevos; el superblock se actualiza
//!   al final. Un corte antes de ese último paso deja nodos huérfanos pero el
//!   estado visible sigue siendo el anterior.
//!
//! No hay heap: la tabla de entradas en reconstrucción es un `static` acotado a
//! 512 elementos (patrón `UnsafeCell` + `unsafe impl Sync` del resto del
//! kernel, mono-CPU con IRQs desactivadas).

use core::cell::UnsafeCell;
use core::fmt;

use meridian_crypto::sha256;

use crate::blk::{self, BlockDeviceId, BlockError, SECTOR_SIZE};
use crate::cap::{CapTable, Rights};

pub mod selftest;

/// Slot A del superblock duplicado.
pub const SUPERBLOCK_A: u64 = 0;
/// Slot B del superblock duplicado.
pub const SUPERBLOCK_B: u64 = 1;
/// Primer LBA que no pertenece a un superblock.
pub const FIRST_DATA_LBA: u64 = 2;

/// Longitud de un SHA-256.
pub const HASH_LEN: usize = 32;
/// Los bytes `0..480` están cubiertos por el checksum; `480..512` lo contienen.
pub const CHECKSUM_OFFSET: usize = SECTOR_SIZE - HASH_LEN;
/// Máximo de entradas por nodo.
pub const FANOUT: usize = 8;
/// Tamaño máximo de un valor/extent.
pub const MAX_VALUE_LEN: usize = 4096;
/// Sectores máximos de un extent.
pub const MAX_VALUE_SECTORS: usize = MAX_VALUE_LEN / SECTOR_SIZE;
/// Capacidad total del árbol (`8 * 8`).
pub const MAX_ENTRIES: usize = FANOUT * FANOUT;

const SUPER_MAGIC: &[u8; 8] = b"MERIDOBJ";
const SUPER_VERSION: u16 = 1;
const NODE_MAGIC: &[u8; 4] = b"OBJN";
const NODE_LEAF: u8 = 1;
const NODE_INTERNAL: u8 = 2;

const SB_VERSION: usize = 8;
const SB_SLOT: usize = 10;
const SB_GENERATION: usize = 16;
const SB_ROOT_LBA: usize = 24;
const SB_NEXT_FREE: usize = 32;
const SB_ROOT_HASH: usize = 40;

const NODE_HEADER_LEN: usize = 8;
const LEAF_ENTRY_LEN: usize = 56;
const INTERNAL_ENTRY_LEN: usize = 48;

/// Errores del sistema de objetos en disco.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjError {
    Block(BlockError),
    NotFormatted,
    BadSuperblock,
    BadNode,
    NodeHashMismatch,
    DataHashMismatch,
    KeyNotFound,
    ValueTooLarge,
    BufferTooSmall,
    TooManyObjects,
    OutOfSpace,
}

impl fmt::Display for ObjError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Block(error) => write!(f, "bloque: {error}"),
            Self::NotFormatted => write!(f, "superblock no formateado"),
            Self::BadSuperblock => write!(f, "superblock inválido"),
            Self::BadNode => write!(f, "nodo inválido"),
            Self::NodeHashMismatch => write!(f, "checksum/hash de nodo incorrecto"),
            Self::DataHashMismatch => write!(f, "hash del dato incorrecto"),
            Self::KeyNotFound => write!(f, "clave no encontrada"),
            Self::ValueTooLarge => write!(f, "valor mayor que 4096 bytes"),
            Self::BufferTooSmall => write!(f, "búfer de salida demasiado pequeño"),
            Self::TooManyObjects => write!(f, "límite de 64 objetos alcanzado"),
            Self::OutOfSpace => write!(f, "sin sectores libres"),
        }
    }
}

impl From<BlockError> for ObjError {
    fn from(error: BlockError) -> Self {
        Self::Block(error)
    }
}

/// Entrada de una hoja, tal como se serializa en disco.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeafEntry {
    key: u64,
    lba: u64,
    len: u32,
    data_hash: [u8; HASH_LEN],
}

impl LeafEntry {
    const EMPTY: Self = Self {
        key: 0,
        lba: 0,
        len: 0,
        data_hash: [0; HASH_LEN],
    };
}

/// Referencia a un nodo hijo (entrada interna).
#[derive(Clone, Copy)]
struct ChildRef {
    min_key: u64,
    lba: u64,
    hash: [u8; HASH_LEN],
}

impl ChildRef {
    const EMPTY: Self = Self {
        min_key: 0,
        lba: 0,
        hash: [0; HASH_LEN],
    };
}

/// Estado persistente cargado de un superblock válido.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectStore {
    pub device: BlockDeviceId,
    pub sectors: u64,
    pub generation: u64,
    pub root_lba: u64,
    pub root_hash: [u8; HASH_LEN],
    pub next_free: u64,
}

/// Datos de la próxima versión del superblock, antes de publicarlos.
#[derive(Clone, Copy)]
struct Pending {
    root_lba: u64,
    root_hash: [u8; HASH_LEN],
    next_free: u64,
}

/// Nodo leído y verificado.
struct Node {
    bytes: [u8; SECTOR_SIZE],
    kind: u8,
    count: usize,
}

/// Asignador monotónico dentro de la generación actual.
struct Allocator {
    next: u64,
    sectors: u64,
}

impl Allocator {
    fn take(&mut self, count: u64) -> Result<u64, ObjError> {
        if count == 0 || count > self.sectors.saturating_sub(self.next) {
            return Err(ObjError::OutOfSpace);
        }
        let lba = self.next;
        self.next += count;
        Ok(lba)
    }
}

struct Scratch {
    entries: [LeafEntry; MAX_ENTRIES],
}

struct GlobalScratch(UnsafeCell<Scratch>);

// SAFETY: kernel mono-CPU con interrupciones desactivadas; `load_entries` y
// `build` son secuenciales y no se reentrantan (mismo modelo que `blk`).
unsafe impl Sync for GlobalScratch {}

static SCRATCH: GlobalScratch = GlobalScratch(UnsafeCell::new(Scratch {
    entries: [LeafEntry::EMPTY; MAX_ENTRIES],
}));

fn scratch() -> &'static mut Scratch {
    // SAFETY: ver `GlobalScratch`.
    unsafe { &mut *SCRATCH.0.get() }
}

fn put_u16(buf: &mut [u8; SECTOR_SIZE], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8; SECTOR_SIZE], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buf: &mut [u8; SECTOR_SIZE], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(buf: &[u8; SECTOR_SIZE], offset: usize) -> u16 {
    let mut bytes = [0u8; 2];
    bytes.copy_from_slice(&buf[offset..offset + 2]);
    u16::from_le_bytes(bytes)
}

fn get_u64(buf: &[u8; SECTOR_SIZE], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn seal(buf: &mut [u8; SECTOR_SIZE]) {
    let sum = sha256::hash(&buf[..CHECKSUM_OFFSET]);
    buf[CHECKSUM_OFFSET..].copy_from_slice(&sum);
}

fn encode_superblock(slot: u16, generation: u64, pending: Pending) -> [u8; SECTOR_SIZE] {
    let mut buf = [0u8; SECTOR_SIZE];
    buf[..8].copy_from_slice(SUPER_MAGIC);
    put_u16(&mut buf, SB_VERSION, SUPER_VERSION);
    put_u16(&mut buf, SB_SLOT, slot);
    put_u64(&mut buf, SB_GENERATION, generation);
    put_u64(&mut buf, SB_ROOT_LBA, pending.root_lba);
    put_u64(&mut buf, SB_NEXT_FREE, pending.next_free);
    buf[SB_ROOT_HASH..SB_ROOT_HASH + HASH_LEN].copy_from_slice(&pending.root_hash);
    seal(&mut buf);
    buf
}

fn decode_superblock(
    buf: &[u8; SECTOR_SIZE],
    slot: u16,
    sectors: u64,
) -> Result<(u64, Pending), ObjError> {
    if &buf[..8] != SUPER_MAGIC || get_u16(buf, SB_VERSION) != SUPER_VERSION {
        return Err(ObjError::BadSuperblock);
    }
    if get_u16(buf, SB_SLOT) != slot {
        return Err(ObjError::BadSuperblock);
    }
    let sum = sha256::hash(&buf[..CHECKSUM_OFFSET]);
    if &sum[..] != &buf[CHECKSUM_OFFSET..] {
        return Err(ObjError::BadSuperblock);
    }
    let mut root_hash = [0u8; HASH_LEN];
    root_hash.copy_from_slice(&buf[SB_ROOT_HASH..SB_ROOT_HASH + HASH_LEN]);
    let pending = Pending {
        root_lba: get_u64(buf, SB_ROOT_LBA),
        root_hash,
        next_free: get_u64(buf, SB_NEXT_FREE),
    };
    if pending.root_lba < FIRST_DATA_LBA
        || pending.root_lba >= sectors
        || pending.next_free <= pending.root_lba
        || pending.next_free > sectors
    {
        return Err(ObjError::BadSuperblock);
    }
    Ok((get_u64(buf, SB_GENERATION), pending))
}

fn seal_node(kind: u8, count: usize, buf: &mut [u8; SECTOR_SIZE]) {
    buf[..4].copy_from_slice(NODE_MAGIC);
    buf[4] = kind;
    buf[5] = count as u8;
    seal(buf);
}

fn encode_leaf(entries: &[LeafEntry]) -> [u8; SECTOR_SIZE] {
    let mut buf = [0u8; SECTOR_SIZE];
    let mut offset = NODE_HEADER_LEN;
    for entry in entries {
        put_u64(&mut buf, offset, entry.key);
        put_u64(&mut buf, offset + 8, entry.lba);
        put_u32(&mut buf, offset + 16, entry.len);
        put_u32(&mut buf, offset + 20, 0);
        buf[offset + 24..offset + LEAF_ENTRY_LEN].copy_from_slice(&entry.data_hash);
        offset += LEAF_ENTRY_LEN;
    }
    seal_node(NODE_LEAF, entries.len(), &mut buf);
    buf
}

fn encode_internal(refs: &[ChildRef]) -> [u8; SECTOR_SIZE] {
    let mut buf = [0u8; SECTOR_SIZE];
    let mut offset = NODE_HEADER_LEN;
    for reference in refs {
        put_u64(&mut buf, offset, reference.min_key);
        put_u64(&mut buf, offset + 8, reference.lba);
        buf[offset + 16..offset + INTERNAL_ENTRY_LEN].copy_from_slice(&reference.hash);
        offset += INTERNAL_ENTRY_LEN;
    }
    seal_node(NODE_INTERNAL, refs.len(), &mut buf);
    buf
}

fn parse_leaf(buf: &[u8; SECTOR_SIZE], index: usize) -> LeafEntry {
    let offset = NODE_HEADER_LEN + index * LEAF_ENTRY_LEN;
    let mut data_hash = [0u8; HASH_LEN];
    data_hash.copy_from_slice(&buf[offset + 24..offset + LEAF_ENTRY_LEN]);
    LeafEntry {
        key: get_u64(buf, offset),
        lba: get_u64(buf, offset + 8),
        len: get_u32(buf, offset + 16),
        data_hash,
    }
}

fn get_u32(buf: &[u8; SECTOR_SIZE], offset: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buf[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

fn parse_internal(buf: &[u8; SECTOR_SIZE], index: usize) -> ChildRef {
    let offset = NODE_HEADER_LEN + index * INTERNAL_ENTRY_LEN;
    let mut hash = [0u8; HASH_LEN];
    hash.copy_from_slice(&buf[offset + 16..offset + INTERNAL_ENTRY_LEN]);
    ChildRef {
        min_key: get_u64(buf, offset),
        lba: get_u64(buf, offset + 8),
        hash,
    }
}

/// Comprueba el checksum del nodo y devuelve `(tipo,entradas)`.
fn inspect_node(buf: &[u8; SECTOR_SIZE]) -> Result<(u8, usize), ObjError> {
    let sum = sha256::hash(&buf[..CHECKSUM_OFFSET]);
    if &sum[..] != &buf[CHECKSUM_OFFSET..] {
        return Err(ObjError::NodeHashMismatch);
    }
    if &buf[..4] != NODE_MAGIC {
        return Err(ObjError::BadNode);
    }
    let kind = buf[4];
    if kind != NODE_LEAF && kind != NODE_INTERNAL {
        return Err(ObjError::BadNode);
    }
    let count = buf[5] as usize;
    if count > FANOUT {
        return Err(ObjError::BadNode);
    }
    Ok((kind, count))
}

fn read_node(
    caps: &CapTable,
    slot: usize,
    device: BlockDeviceId,
    lba: u64,
    expected_hash: Option<&[u8; HASH_LEN]>,
) -> Result<Node, ObjError> {
    let mut bytes = [0u8; SECTOR_SIZE];
    blk::read_sector(caps, slot, device, lba, &mut bytes)?;
    let (kind, count) = inspect_node(&bytes)?;
    if let Some(expected) = expected_hash {
        let actual = sha256::hash(&bytes);
        if &actual[..] != &expected[..] {
            return Err(ObjError::NodeHashMismatch);
        }
    }
    Ok(Node { bytes, kind, count })
}

fn select_child(node: &Node, key: u64) -> ChildRef {
    let mut chosen = parse_internal(&node.bytes, 0);
    let mut index = 1;
    while index < node.count {
        let candidate = parse_internal(&node.bytes, index);
        if candidate.min_key <= key {
            chosen = candidate;
        }
        index += 1;
    }
    chosen
}

impl ObjectStore {
    /// Formatea el dispositivo: escribe una raíz vacía en el LBA 2 y ambos
    /// superblocks con generación 1.
    pub fn format(caps: &CapTable, slot: usize, device: BlockDeviceId) -> Result<Self, ObjError> {
        let sectors = blk::sector_count(device);
        if sectors <= FIRST_DATA_LBA {
            return Err(ObjError::OutOfSpace);
        }
        caps.check(slot, Rights::WRITE).map_err(BlockError::from)?;

        let root = encode_internal(&[]);
        let root_hash = sha256::hash(&root);
        blk::write_sector(caps, slot, device, FIRST_DATA_LBA, &root)?;
        let pending = Pending {
            root_lba: FIRST_DATA_LBA,
            root_hash,
            next_free: FIRST_DATA_LBA + 1,
        };
        for (index, lba) in [SUPERBLOCK_A, SUPERBLOCK_B].into_iter().enumerate() {
            let block = encode_superblock(index as u16, 1, pending);
            blk::write_sector(caps, slot, device, lba, &block)?;
        }
        Ok(Self {
            device,
            sectors,
            generation: 1,
            root_lba: pending.root_lba,
            root_hash: pending.root_hash,
            next_free: pending.next_free,
        })
    }

    /// Valida los dos superblocks y elige el de mayor generación.
    pub fn open(caps: &CapTable, slot: usize, device: BlockDeviceId) -> Result<Self, ObjError> {
        let sectors = blk::sector_count(device);
        if sectors <= FIRST_DATA_LBA {
            return Err(ObjError::NotFormatted);
        }
        let mut best: Option<(u64, Pending)> = None;
        let mut index = 0u16;
        while index < 2 {
            let lba = if index == 0 {
                SUPERBLOCK_A
            } else {
                SUPERBLOCK_B
            };
            let mut block = [0u8; SECTOR_SIZE];
            if blk::read_sector(caps, slot, device, lba, &mut block).is_ok() {
                if let Ok((generation, pending)) = decode_superblock(&block, index, sectors) {
                    let root = read_node(
                        caps,
                        slot,
                        device,
                        pending.root_lba,
                        Some(&pending.root_hash),
                    );
                    if root.map_or(false, |node| node.kind == NODE_INTERNAL)
                        && best.map_or(true, |(current, _)| generation > current)
                    {
                        best = Some((generation, pending));
                    }
                }
            }
            index += 1;
        }
        let (generation, pending) = best.ok_or(ObjError::NotFormatted)?;
        Ok(Self {
            device,
            sectors,
            generation,
            root_lba: pending.root_lba,
            root_hash: pending.root_hash,
            next_free: pending.next_free,
        })
    }

    /// Devuelve `(lba,len)` del extent de una clave (ayuda de inspección/QA).
    pub fn locate(
        &self,
        caps: &CapTable,
        slot: usize,
        key: u64,
    ) -> Result<Option<(u64, u32)>, ObjError> {
        Ok(self
            .find_entry(caps, slot, key)?
            .map(|entry| (entry.lba, entry.len)))
    }

    /// Lee un objeto y verifica el SHA-256 de sus bytes útiles.
    /// Devuelve el número de bytes copiados en `out`.
    pub fn read(
        &self,
        caps: &CapTable,
        slot: usize,
        key: u64,
        out: &mut [u8],
    ) -> Result<usize, ObjError> {
        let entry = self
            .find_entry(caps, slot, key)?
            .ok_or(ObjError::KeyNotFound)?;
        let len = entry.len as usize;
        if out.len() < len {
            return Err(ObjError::BufferTooSmall);
        }
        let count = (len + SECTOR_SIZE - 1) / SECTOR_SIZE;
        if count > MAX_VALUE_SECTORS {
            return Err(ObjError::BadNode);
        }
        let mut hasher = sha256::Sha256::new();
        let mut block = [0u8; SECTOR_SIZE];
        let mut copied = 0usize;
        let mut index = 0usize;
        while index < count {
            blk::read_sector(
                caps,
                slot,
                self.device,
                entry.lba + index as u64,
                &mut block,
            )?;
            let take = core::cmp::min(SECTOR_SIZE, len - copied);
            hasher.update(&block[..take]);
            out[copied..copied + take].copy_from_slice(&block[..take]);
            copied += take;
            index += 1;
        }
        let mut digest = [0u8; HASH_LEN];
        hasher.finalize(&mut digest);
        if &digest[..] != &entry.data_hash[..] {
            return Err(ObjError::DataHashMismatch);
        }
        Ok(len)
    }

    /// Escritura copy-on-write: construye el árbol nuevo en sectores nuevos y
    /// publica el superblock al final. Nunca sobrescribe nodos/data vivos.
    pub fn write(
        &mut self,
        caps: &CapTable,
        slot: usize,
        key: u64,
        data: &[u8],
    ) -> Result<(), ObjError> {
        let pending = self.build(caps, slot, key, data)?;
        self.commit(caps, slot, pending)
    }

    /// Igual que [`Self::write`] pero **sin** actualizar el superblock.
    /// Simula un corte de energía justo antes del commit: los nodos nuevos
    /// quedan en disco, pero un `open` posterior sigue viendo el estado
    /// anterior y `self` no se modifica.
    pub fn write_uncommitted(
        &mut self,
        caps: &CapTable,
        slot: usize,
        key: u64,
        data: &[u8],
    ) -> Result<(), ObjError> {
        let _pending = self.build(caps, slot, key, data)?;
        Ok(())
    }

    fn find_entry(
        &self,
        caps: &CapTable,
        slot: usize,
        key: u64,
    ) -> Result<Option<LeafEntry>, ObjError> {
        let root = read_node(
            caps,
            slot,
            self.device,
            self.root_lba,
            Some(&self.root_hash),
        )?;
        if root.kind != NODE_INTERNAL || root.count == 0 {
            return Ok(None);
        }
        let mid_ref = select_child(&root, key);
        let mid = read_node(caps, slot, self.device, mid_ref.lba, Some(&mid_ref.hash))?;
        if mid.kind != NODE_INTERNAL || mid.count == 0 {
            return Ok(None);
        }
        let leaf_ref = select_child(&mid, key);
        let leaf = read_node(caps, slot, self.device, leaf_ref.lba, Some(&leaf_ref.hash))?;
        if leaf.kind != NODE_LEAF {
            return Err(ObjError::BadNode);
        }
        let mut index = 0;
        while index < leaf.count {
            let entry = parse_leaf(&leaf.bytes, index);
            if entry.key == key {
                return Ok(Some(entry));
            }
            index += 1;
        }
        Ok(None)
    }

    /// Copia todas las entradas vivas del árbol al scratch (en orden de clave).
    fn load_entries(&self, caps: &CapTable, slot: usize) -> Result<usize, ObjError> {
        let root = read_node(
            caps,
            slot,
            self.device,
            self.root_lba,
            Some(&self.root_hash),
        )?;
        if root.kind != NODE_INTERNAL {
            return Err(ObjError::BadNode);
        }
        let scratch = scratch();
        let mut count = 0usize;
        let mut i = 0;
        while i < root.count {
            let mid_ref = parse_internal(&root.bytes, i);
            let mid = read_node(caps, slot, self.device, mid_ref.lba, Some(&mid_ref.hash))?;
            if mid.kind != NODE_INTERNAL {
                return Err(ObjError::BadNode);
            }
            let mut j = 0;
            while j < mid.count {
                let leaf_ref = parse_internal(&mid.bytes, j);
                let leaf = read_node(caps, slot, self.device, leaf_ref.lba, Some(&leaf_ref.hash))?;
                if leaf.kind != NODE_LEAF {
                    return Err(ObjError::BadNode);
                }
                let mut k = 0;
                while k < leaf.count {
                    if count >= MAX_ENTRIES {
                        return Err(ObjError::TooManyObjects);
                    }
                    scratch.entries[count] = parse_leaf(&leaf.bytes, k);
                    count += 1;
                    k += 1;
                }
                j += 1;
            }
            i += 1;
        }
        Ok(count)
    }

    /// Escribe valor y nodos nuevos; devuelve el estado a publicar.
    fn build(
        &self,
        caps: &CapTable,
        slot: usize,
        key: u64,
        data: &[u8],
    ) -> Result<Pending, ObjError> {
        if data.len() > MAX_VALUE_LEN {
            return Err(ObjError::ValueTooLarge);
        }
        let sectors = (data.len() + SECTOR_SIZE - 1) / SECTOR_SIZE;
        let mut alloc = Allocator {
            next: self.next_free,
            sectors: self.sectors,
        };

        // 1. Extent nuevo, contiguo, con el hash de los bytes útiles.
        let value_lba = if sectors == 0 {
            self.next_free
        } else {
            alloc.take(sectors as u64)?
        };
        let mut hasher = sha256::Sha256::new();
        let mut block = [0u8; SECTOR_SIZE];
        let mut offset = 0usize;
        let mut index = 0usize;
        while index < sectors {
            block.fill(0);
            let take = core::cmp::min(SECTOR_SIZE, data.len() - offset);
            block[..take].copy_from_slice(&data[offset..offset + take]);
            hasher.update(&block[..take]);
            blk::write_sector(caps, slot, self.device, value_lba + index as u64, &block)?;
            offset += take;
            index += 1;
        }
        let mut data_hash = [0u8; HASH_LEN];
        hasher.finalize(&mut data_hash);

        // 2. Se copian las entradas del estado comprometido (nunca de los
        //    sectores recién escritos) y se inserta/reemplaza la clave.
        let count = self.load_entries(caps, slot)?;
        let scratch = scratch();
        let mut position = 0usize;
        while position < count && scratch.entries[position].key < key {
            position += 1;
        }
        let entry = LeafEntry {
            key,
            lba: value_lba,
            len: data.len() as u32,
            data_hash,
        };
        let replaced = position < count && scratch.entries[position].key == key;
        if replaced {
            scratch.entries[position] = entry;
        } else {
            if count >= MAX_ENTRIES {
                return Err(ObjError::TooManyObjects);
            }
            let mut at = count;
            while at > position {
                scratch.entries[at] = scratch.entries[at - 1];
                at -= 1;
            }
            scratch.entries[position] = entry;
        }
        let new_count = count + usize::from(!replaced);
        if new_count > MAX_ENTRIES {
            return Err(ObjError::TooManyObjects);
        }

        // 3. Reconstrucción bottom-up en sectores nuevos: hojas (8 entradas),
        //    nodos internos (8 hojas) y raíz.
        let mut mid_refs = [ChildRef::EMPTY; FANOUT];
        let mut mid_count = 0usize;
        let mut base = 0usize;
        while base < new_count {
            let mut leaf_refs = [ChildRef::EMPTY; FANOUT];
            let mut leaf_count = 0usize;
            let mut cursor = base;
            while cursor < new_count && leaf_count < FANOUT {
                let end = core::cmp::min(cursor + FANOUT, new_count);
                let node = encode_leaf(&scratch.entries[cursor..end]);
                let lba = alloc.take(1)?;
                blk::write_sector(caps, slot, self.device, lba, &node)?;
                leaf_refs[leaf_count] = ChildRef {
                    min_key: scratch.entries[cursor].key,
                    lba,
                    hash: sha256::hash(&node),
                };
                leaf_count += 1;
                cursor = end;
            }
            if mid_count >= FANOUT {
                return Err(ObjError::TooManyObjects);
            }
            let node = encode_internal(&leaf_refs[..leaf_count]);
            let lba = alloc.take(1)?;
            blk::write_sector(caps, slot, self.device, lba, &node)?;
            mid_refs[mid_count] = ChildRef {
                min_key: leaf_refs[0].min_key,
                lba,
                hash: sha256::hash(&node),
            };
            mid_count += 1;
            base = cursor;
        }
        let root = encode_internal(&mid_refs[..mid_count]);
        let root_lba = alloc.take(1)?;
        blk::write_sector(caps, slot, self.device, root_lba, &root)?;

        Ok(Pending {
            root_lba,
            root_hash: sha256::hash(&root),
            next_free: alloc.next,
        })
    }

    /// Publica `pending`: incrementa la generación y escribe solo el slot
    /// `generation % 2`, dejando el otro como fallback.
    fn commit(&mut self, caps: &CapTable, slot: usize, pending: Pending) -> Result<(), ObjError> {
        let generation = self.generation.checked_add(1).ok_or(ObjError::OutOfSpace)?;
        let target = (generation % 2) as u16;
        let lba = if target == 0 {
            SUPERBLOCK_A
        } else {
            SUPERBLOCK_B
        };
        let block = encode_superblock(target, generation, pending);
        blk::write_sector(caps, slot, self.device, lba, &block)?;

        self.generation = generation;
        self.root_lba = pending.root_lba;
        self.root_hash = pending.root_hash;
        self.next_free = pending.next_free;
        Ok(())
    }
}
