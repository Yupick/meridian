//! Argon2id (versión 0x13) según RFC 9106.
//!
//! Implementación `no_std`, sin asignación dinámica y sin `unsafe`: la memoria
//! de trabajo la aporta el llamador en `workspace`. Cada bloque ocupa 1024
//! bytes y se interpreta como 128 palabras de 64 bits en little-endian.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Argon2Error {
    InvalidParameters,
    WorkspaceTooSmall,
}

/// Tamaño de un bloque de memoria de Argon2, en bytes.
const BLOCK_BYTES: usize = 1024;
/// Cantidad de palabras `u64` en un bloque.
const BLOCK_WORDS: usize = 128;
/// Cantidad de slices (puntos de sincronización) por pasada.
const SYNC_POINTS: usize = 4;
/// Cantidad de valores pseudoaleatorios de 64 bits por bloque de direcciones.
const ADDRESSES_PER_BLOCK: usize = 128;
const VERSION: u32 = 0x13;
const TYPE_ARGON2ID: u32 = 2;

/// Devuelve la cantidad de bloques de 1024 bytes que exige Argon2id para los
/// parámetros dados, o `None` si son inválidos o el tamaño desborda `usize`.
///
/// Se redondea `memory_kib` hacia abajo a un múltiplo de `8 * lanes`, tal que
/// cada segmento tenga una cantidad entera de bloques.
pub fn blocks_required(memory_kib: u32, lanes: u32) -> Option<usize> {
    if lanes == 0 {
        return None;
    }
    let lanes = lanes as usize;
    let group = 8usize.checked_mul(lanes)?;
    let memory = memory_kib as usize;
    if memory < group {
        return None;
    }
    let blocks = (memory / group).checked_mul(group)?;
    // El workspace correspondiente debe ser representable en `usize`.
    blocks.checked_mul(BLOCK_BYTES)?;
    Some(blocks)
}

/// Lee un bloque serializado en little-endian como 128 palabras `u64`.
fn load_block(bytes: &[u8]) -> [u64; BLOCK_WORDS] {
    let mut words = [0u64; BLOCK_WORDS];
    for (i, word) in words.iter_mut().enumerate() {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
        *word = u64::from_le_bytes(buf);
    }
    words
}

/// Escribe 128 palabras `u64` como un bloque little-endian.
fn store_block(bytes: &mut [u8], words: &[u64; BLOCK_WORDS]) {
    for (i, word) in words.iter().enumerate() {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
}

/// Lee una palabra `u64` little-endian de un bloque serializado.
fn read_word(bytes: &[u8], index: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[index * 8..index * 8 + 8]);
    u64::from_le_bytes(buf)
}

/// Memoria de Argon2 vista como matriz de `lanes` filas y `lane_length`
/// columnas, partida en `SYNC_POINTS` segmentos por fila.
struct Memory<'a> {
    bytes: &'a mut [u8],
    lanes: usize,
    lane_length: usize,
    segment_length: usize,
}

impl<'a> Memory<'a> {
    fn new(bytes: &'a mut [u8], lanes: usize, blocks: usize) -> Self {
        let lane_length = blocks / lanes;
        Self {
            bytes,
            lanes,
            lane_length,
            segment_length: lane_length / SYNC_POINTS,
        }
    }

    fn block(&self, index: usize) -> &[u8] {
        let start = index * BLOCK_BYTES;
        &self.bytes[start..start + BLOCK_BYTES]
    }

    fn block_mut(&mut self, index: usize) -> &mut [u8] {
        let start = index * BLOCK_BYTES;
        &mut self.bytes[start..start + BLOCK_BYTES]
    }
}

/// Mezcla usada por `GB`: `a + b + 2 * trunc(a) * trunc(b) (mod 2^64)`.
#[inline(always)]
fn mix(a: u64, b: u64) -> u64 {
    let product = (a as u32 as u64).wrapping_mul(b as u32 as u64);
    a.wrapping_add(b).wrapping_add(product.wrapping_mul(2))
}

/// Ronda BLAKE2b sobre cuatro palabras de un estado de 16 palabras.
#[inline(always)]
fn gb(v: &mut [u64; BLOCK_WORDS], a: usize, b: usize, c: usize, d: usize) {
    let mut va = v[a];
    let mut vb = v[b];
    let mut vc = v[c];
    let mut vd = v[d];

    va = mix(va, vb);
    vd = (vd ^ va).rotate_right(32);
    vc = mix(vc, vd);
    vb = (vb ^ vc).rotate_right(24);
    va = mix(va, vb);
    vd = (vd ^ va).rotate_right(16);
    vc = mix(vc, vd);
    vb = (vb ^ vc).rotate_right(63);

    v[a] = va;
    v[b] = vb;
    v[c] = vc;
    v[d] = vd;
}

/// Permutación P de Argon2 (ocho llamadas a `GB`) sobre 16 índices.
fn permute(v: &mut [u64; BLOCK_WORDS], idx: [usize; 16]) {
    gb(v, idx[0], idx[4], idx[8], idx[12]);
    gb(v, idx[1], idx[5], idx[9], idx[13]);
    gb(v, idx[2], idx[6], idx[10], idx[14]);
    gb(v, idx[3], idx[7], idx[11], idx[15]);
    gb(v, idx[0], idx[5], idx[10], idx[15]);
    gb(v, idx[1], idx[6], idx[11], idx[12]);
    gb(v, idx[2], idx[7], idx[8], idx[13]);
    gb(v, idx[3], idx[4], idx[9], idx[14]);
}

/// Compresión G(X, Y) de la RFC 9106: `R = X xor Y`, P por filas y por
/// columnas, y resultado `R xor P(P(R))`. Si `old` está presente, se XORa el
/// bloque previo (pasadas posteriores a la primera).
fn compress(
    prev: &[u64; BLOCK_WORDS],
    reference: &[u64; BLOCK_WORDS],
    old: Option<&[u64; BLOCK_WORDS]>,
) -> [u64; BLOCK_WORDS] {
    let mut r = [0u64; BLOCK_WORDS];
    let mut out = [0u64; BLOCK_WORDS];
    for i in 0..BLOCK_WORDS {
        r[i] = prev[i] ^ reference[i];
        out[i] = r[i];
    }
    if let Some(old) = old {
        for i in 0..BLOCK_WORDS {
            out[i] ^= old[i];
        }
    }

    for row in 0..8 {
        let mut idx = [0usize; 16];
        for j in 0..16 {
            idx[j] = row * 16 + j;
        }
        permute(&mut r, idx);
    }
    for col in 0..8 {
        let mut idx = [0usize; 16];
        for k in 0..8 {
            idx[2 * k] = 16 * k + 2 * col;
            idx[2 * k + 1] = 16 * k + 2 * col + 1;
        }
        permute(&mut r, idx);
    }

    for i in 0..BLOCK_WORDS {
        out[i] ^= r[i];
    }
    out
}

/// Genera un nuevo bloque de direcciones: `G(0, G(0, input))`.
fn next_addresses(input: &[u64; BLOCK_WORDS]) -> [u64; BLOCK_WORDS] {
    let zero = [0u64; BLOCK_WORDS];
    let first = compress(&zero, input, None);
    compress(&zero, &first, None)
}

/// Mapea `J1` (32 bits bajos) a un índice absoluto dentro de la fila, según
/// RFC 9106 sección 3.4.2.
fn index_alpha(
    j1: u32,
    pass: u32,
    slice: usize,
    index: usize,
    same_lane: bool,
    lane_length: usize,
    segment_length: usize,
) -> usize {
    let reference_area_size: u64 = if pass == 0 {
        if slice == 0 {
            (index - 1) as u64
        } else if same_lane {
            (slice * segment_length + index - 1) as u64
        } else {
            (slice * segment_length - usize::from(index == 0)) as u64
        }
    } else if same_lane {
        (lane_length - segment_length + index - 1) as u64
    } else {
        (lane_length - segment_length - usize::from(index == 0)) as u64
    };

    let pr = j1 as u64;
    let x = (pr * pr) >> 32;
    let y = (reference_area_size * x) >> 32;
    let relative = reference_area_size - 1 - y;

    let start = if pass != 0 {
        if slice == SYNC_POINTS - 1 {
            0
        } else {
            (slice + 1) * segment_length
        }
    } else {
        0
    };
    (start + relative as usize) % lane_length
}

/// Escribe un bloque de memoria con el resultado de la compresión.
fn fill_block(
    mem: &mut Memory<'_>,
    prev_index: usize,
    ref_index: usize,
    curr_index: usize,
    with_xor: bool,
) {
    let prev = load_block(mem.block(prev_index));
    let reference = load_block(mem.block(ref_index));
    let result = if with_xor {
        let old = load_block(mem.block(curr_index));
        compress(&prev, &reference, Some(&old))
    } else {
        compress(&prev, &reference, None)
    };
    store_block(mem.block_mut(curr_index), &result);
}

/// Rellena un segmento (intersección de un slice y un lane).
fn fill_segment(mem: &mut Memory<'_>, pass: u32, lane: usize, slice: usize, iterations: u32) {
    let lane_length = mem.lane_length;
    let segment_length = mem.segment_length;
    // Argon2id: direccionamiento independiente de los datos solo en la
    // primera mitad del primer pase.
    let data_independent = pass == 0 && slice < SYNC_POINTS / 2;
    let starting_index = if pass == 0 && slice == 0 { 2 } else { 0 };

    let mut input = [0u64; BLOCK_WORDS];
    if data_independent {
        input[0] = pass as u64;
        input[1] = lane as u64;
        input[2] = slice as u64;
        input[3] = (mem.bytes.len() / BLOCK_BYTES) as u64;
        input[4] = iterations as u64;
        input[5] = TYPE_ARGON2ID as u64;
    }
    let mut counter = 0u64;
    let mut address = [0u64; BLOCK_WORDS];

    let mut curr_offset = lane * lane_length + slice * segment_length + starting_index;
    let mut prev_offset = if curr_offset % lane_length == 0 {
        curr_offset + lane_length - 1
    } else {
        curr_offset - 1
    };

    if data_independent && starting_index == 2 {
        counter += 1;
        input[6] = counter;
        address = next_addresses(&input);
    }

    for i in starting_index..segment_length {
        if curr_offset % lane_length == 1 {
            prev_offset = curr_offset - 1;
        }

        let pseudo_rand = if data_independent {
            if i % ADDRESSES_PER_BLOCK == 0 {
                counter += 1;
                input[6] = counter;
                address = next_addresses(&input);
            }
            address[i % ADDRESSES_PER_BLOCK]
        } else {
            read_word(mem.block(prev_offset), 0)
        };

        let mut ref_lane = ((pseudo_rand >> 32) as usize) % mem.lanes;
        if pass == 0 && slice == 0 {
            // En el primer slice del primer pase aún no hay otras filas listas.
            ref_lane = lane;
        }
        let ref_index = index_alpha(
            pseudo_rand as u32,
            pass,
            slice,
            i,
            ref_lane == lane,
            lane_length,
            segment_length,
        );
        let ref_index_global = ref_lane * lane_length + ref_index;
        fill_block(mem, prev_offset, ref_index_global, curr_offset, pass != 0);

        curr_offset += 1;
        prev_offset += 1;
    }
}

/// Calcula un tag Argon2id de `out.len()` bytes.
pub fn argon2id(
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    associated_data: &[u8],
    iterations: u32,
    lanes: u32,
    memory_kib: u32,
    workspace: &mut [u8],
    out: &mut [u8],
) -> Result<(), Argon2Error> {
    if iterations == 0 || lanes == 0 || salt.len() < 8 || out.is_empty() {
        return Err(Argon2Error::InvalidParameters);
    }
    let max_len = u32::MAX as usize;
    if password.len() > max_len
        || salt.len() > max_len
        || secret.len() > max_len
        || associated_data.len() > max_len
        || out.len() > max_len
    {
        return Err(Argon2Error::InvalidParameters);
    }

    let blocks = blocks_required(memory_kib, lanes).ok_or(Argon2Error::InvalidParameters)?;
    let needed = blocks
        .checked_mul(BLOCK_BYTES)
        .ok_or(Argon2Error::InvalidParameters)?;
    if workspace.len() < needed {
        return Err(Argon2Error::WorkspaceTooSmall);
    }

    // H_0 = H^64(LE32(p) || LE32(T) || LE32(m) || LE32(t) ||
    //             LE32(v) || LE32(y) || LE32(|P|) || P || ...)
    let mut hasher = crate::blake2b::Blake2bHasher::new(64);
    hasher.update(&lanes.to_le_bytes());
    hasher.update(&(out.len() as u32).to_le_bytes());
    hasher.update(&memory_kib.to_le_bytes());
    hasher.update(&iterations.to_le_bytes());
    hasher.update(&VERSION.to_le_bytes());
    hasher.update(&TYPE_ARGON2ID.to_le_bytes());
    hasher.update(&(password.len() as u32).to_le_bytes());
    hasher.update(password);
    hasher.update(&(salt.len() as u32).to_le_bytes());
    hasher.update(salt);
    hasher.update(&(secret.len() as u32).to_le_bytes());
    hasher.update(secret);
    hasher.update(&(associated_data.len() as u32).to_le_bytes());
    hasher.update(associated_data);
    let mut h0 = [0u8; 64];
    hasher
        .finalize(&mut h0)
        .map_err(|_| Argon2Error::InvalidParameters)?;

    let lane_count = lanes as usize;
    let mut mem = Memory::new(&mut workspace[..needed], lane_count, blocks);

    // Bloques iniciales de cada fila: H'^1024(H_0 || LE32(j) || LE32(i)).
    for lane in 0..lane_count {
        for j in 0..2u32 {
            let mut seed = [0u8; 72];
            seed[..64].copy_from_slice(&h0);
            seed[64..68].copy_from_slice(&j.to_le_bytes());
            seed[68..72].copy_from_slice(&(lane as u32).to_le_bytes());
            let index = lane * mem.lane_length + j as usize;
            crate::blake2b::variable(1024, &seed, mem.block_mut(index))
                .map_err(|_| Argon2Error::InvalidParameters)?;
        }
    }

    // Llenado por pasadas, slices y segmentos (orden slicewise de la RFC).
    for pass in 0..iterations {
        for slice in 0..SYNC_POINTS {
            for lane in 0..lane_count {
                fill_segment(&mut mem, pass, lane, slice, iterations);
            }
        }
    }

    // C = B[0][q-1] xor ... xor B[p-1][q-1]; tag = H'^T(C).
    let mut final_block = [0u64; BLOCK_WORDS];
    let last_column = mem.lane_length - 1;
    for lane in 0..lane_count {
        let block = load_block(mem.block(lane * mem.lane_length + last_column));
        for i in 0..BLOCK_WORDS {
            final_block[i] ^= block[i];
        }
    }
    let mut final_bytes = [0u8; BLOCK_BYTES];
    store_block(&mut final_bytes, &final_block);
    crate::blake2b::variable(out.len(), &final_bytes, out)
        .map_err(|_| Argon2Error::InvalidParameters)
}
