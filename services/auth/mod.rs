//! Servicio de login y usuarios (A8, alpha).
//!
//! Almacén de usuarios en memoria con hash de contraseña **temporal** (NO apto
//! para producción). En la fase 1 se sustituirá por Argon2id propio validado.
//! Cada sesión autenticada recibe una tabla de capacidades y un directorio
//! personal.

#![allow(dead_code)]

use core::fmt;

use crate::cap::{CapTable, Capability, Rights};
use crate::fs::{self, FsError, FS_OBJECT_ID, ROOT_NODE, NodeId};

pub mod selftest;

/// Bits de grupo para las ACLs del volumen cifrado (T2.2).
pub const GROUP_ADMIN: u8 = 1 << 0;
pub const GROUP_USERS: u8 = 1 << 1;
pub const GROUP_AUDIT: u8 = 1 << 2;
pub const GROUP_GUEST: u8 = 1 << 3;
/// Todos los grupos definidos (lectura pública).
pub const GROUP_ALL: u8 = GROUP_ADMIN | GROUP_USERS | GROUP_AUDIT | GROUP_GUEST;

/// Máximo de usuarios registrados.
pub const MAX_USERS: usize = 8;
pub const MAX_NAME: usize = 16;
/// Longitud del hash almacenado (bytes).
pub const HASH_LEN: usize = 32;
/// Slot de capacidad del sistema de archivos en la sesión del usuario.
pub const SESSION_FS_SLOT: usize = 0;

/// Error de autenticación genérico (no revela si el usuario existe).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Usuario inexistente o contraseña incorrecta.
    InvalidCredentials,
    /// Almacén de usuarios lleno.
    NoSpace,
    /// Nombre inválido.
    InvalidName,
    /// Error del sistema de archivos al preparar el directorio personal.
    Fs,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCredentials => write!(f, "credenciales inválidas"),
            Self::NoSpace => write!(f, "sin espacio para usuarios"),
            Self::InvalidName => write!(f, "nombre de usuario inválido"),
            Self::Fs => write!(f, "error de sistema de archivos"),
        }
    }
}

impl From<FsError> for AuthError {
    fn from(_: FsError) -> Self {
        Self::Fs
    }
}

/// Hash temporal de contraseña (FNV-1a 64-bit iterado + sal), **NO CRIPTOGRÁFICO**.
/// Marcado explícitamente como no apto para producción; se reemplazará por
/// Argon2id propio validado en la fase 1 (T1.4).
fn temp_hash(password: &[u8], salt: u64) -> [u8; HASH_LEN] {
    // Derivación determinista simple para alpha: no ofrece resistencia real.
    let mut out = [0u8; HASH_LEN];
    let mut state: u64 = 0xcbf29ce484222325 ^ salt;
    let mut i = 0usize;
    // Varias rondas mezclando bytes y posición para producir HASH_LEN bytes.
    let mut round: u64 = 0;
    while i < HASH_LEN {
        // Una contraseña vacía es válida como entrada (debe rechazarse en
        // `login`, no provocar un panic al indexar un slice vacío).
        let p = if password.is_empty() {
            0
        } else {
            password[i % password.len()]
        };
        state ^= p as u64;
        state = state.wrapping_mul(0x100000001b3);
        state ^= round.wrapping_add(i as u64);
        state = state.rotate_left(27).wrapping_add(0x9e3779b97f4a7c15);
        out[i] = (state & 0xff) as u8;
        i += 1;
        round = round.wrapping_add(0x517cc1b727220a95);
    }
    // Segunda pasada para difundir.
    i = 0;
    let mut acc = state ^ 0xa5a5a5a5a5a5a5a5;
    while i < HASH_LEN {
        acc ^= (out[i] as u64).wrapping_mul(0x100000001b3 ^ (i as u64));
        acc = acc.rotate_left(17).wrapping_add(salt ^ (i as u64 * 0x9e37));
        out[i] ^= (acc & 0xff) as u8;
        i += 1;
    }
    out
}

/// Grupos inferidos por nombre en alpha (T2.2). El nombre `root` recibe
/// `ADMIN|USERS`, `bob` pertenece además a `AUDIT` y `guest` solo a `GUEST`.
fn groups_for_name(name: &[u8]) -> u8 {
    if name == b"root" {
        GROUP_ADMIN | GROUP_USERS
    } else if name == b"alice" {
        GROUP_USERS
    } else if name == b"bob" {
        GROUP_USERS | GROUP_AUDIT
    } else if name == b"guest" {
        GROUP_GUEST
    } else {
        GROUP_USERS
    }
}

fn eq_const(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff: u8 = 0;
    let mut i = 0;
    while i < a.len() {
        diff |= a[i] ^ b[i];
        i += 1;
    }
    diff == 0
}

#[derive(Clone, Copy)]
struct User {
    in_use: bool,
    name: [u8; MAX_NAME],
    name_len: u8,
    groups: u8,
    salt: u64,
    hash: [u8; HASH_LEN],
    home: [u8; 32],
    home_len: u8,
}

impl User {
    const fn empty() -> Self {
        Self {
            in_use: false,
            name: [0; MAX_NAME],
            name_len: 0,
            groups: 0,
            salt: 0,
            hash: [0; HASH_LEN],
            home: [0; 32],
            home_len: 0,
        }
    }

    fn name_bytes(&self) -> &[u8] { &self.name[..self.name_len as usize] }
    fn home_str_bytes(&self) -> &[u8] { &self.home[..self.home_len as usize] }
}

struct UserStore {
    users: [User; MAX_USERS],
    count: usize,
}

impl UserStore {
    const fn new() -> Self {
        Self { users: [User::empty(); MAX_USERS], count: 0 }
    }

    fn clear(&mut self) {
        let mut i = 0;
        while i < MAX_USERS { self.users[i] = User::empty(); i += 1; }
        self.count = 0;
    }

    fn find(&self, name: &[u8]) -> Option<usize> {
        let mut i = 0;
        while i < MAX_USERS {
            let u = &self.users[i];
            if u.in_use && u.name_len as usize == name.len() && &u.name[..u.name_len as usize] == name {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn add(&mut self, name: &str, password: &str, home: &str, salt: u64) -> Result<(), AuthError> {
        let nb = name.as_bytes();
        if nb.is_empty() || nb.len() > MAX_NAME { return Err(AuthError::InvalidName); }
        if self.find(nb).is_some() { return Err(AuthError::NoSpace); }
        let mut slot = None;
        let mut i = 0;
        while i < MAX_USERS { if !self.users[i].in_use { slot = Some(i); break; } i += 1; }
        let idx = match slot { Some(s) => s, None => return Err(AuthError::NoSpace) };
        let u = &mut self.users[idx];
        *u = User::empty();
        u.in_use = true;
        u.name_len = nb.len() as u8;
        let mut j = 0; while j < nb.len() { u.name[j] = nb[j]; j += 1; }
        u.groups = groups_for_name(nb);
        u.salt = salt;
        u.hash = temp_hash(password.as_bytes(), salt);
        let hb = home.as_bytes();
        let hl = if hb.len() > 32 { 32 } else { hb.len() };
        u.home_len = hl as u8;
        j = 0; while j < hl { u.home[j] = hb[j]; j += 1; }
        self.count += 1;
        Ok(())
    }
}

// Estado global simple (kernel mono-CPU, sin reentrancia).
static mut STORE: UserStore = UserStore::new();

fn store() -> &'static mut UserStore {
    // SAFETY: acceso secuencial en kernel mono-CPU sin interrupciones/reentrancia.
    unsafe { &mut *(&raw mut STORE) }
}

/// Sesión autenticada.
#[derive(Clone, Copy)]
pub struct Session {
    pub username: [u8; MAX_NAME],
    pub username_len: u8,
    /// Grupos de T2.2 inferidos al registrar el usuario.
    pub groups: u8,
    pub caps: CapTable,
    pub home: [u8; 32],
    pub home_len: u8,
    pub cwd: NodeId,
}

impl Session {
    pub fn username_bytes(&self) -> &[u8] { &self.username[..self.username_len as usize] }
    pub fn home_bytes(&self) -> &[u8] { &self.home[..self.home_len as usize] }
}

/// Reinicia el almacén de usuarios (para pruebas).
pub fn reset_users() {
    store().clear();
}

/// Registra un usuario y crea su directorio personal bajo `/home/<name>`.
/// Requiere un contexto con capacidad de escritura al FS.
pub fn register_user(
    fs_caps: &CapTable,
    fs_slot: usize,
    name: &str,
    password: &str,
    salt: u64,
) -> Result<(), AuthError> {
    // Preparar /home y /home/<name>
    fs::ensure_init();
    // Crear /home si no existe
    match fs::mkdir(fs_caps, fs_slot, ROOT_NODE, "/home") {
        Ok(_) | Err(FsError::AlreadyExists) => {}
        Err(e) => return Err(AuthError::from(e)),
    }
    // Construir ruta home
    let mut home_buf = [0u8; 32];
    let prefix = b"/home/";
    let nb = name.as_bytes();
    let total = prefix.len() + nb.len();
    if total > home_buf.len() { return Err(AuthError::InvalidName); }
    let mut k = 0; while k < prefix.len() { home_buf[k] = prefix[k]; k += 1; }
    let mut m = 0; while m < nb.len() { home_buf[k] = nb[m]; k += 1; m += 1; }
    // SAFETY: ASCII
    let home_str = core::str::from_utf8(&home_buf[..total]).map_err(|_| AuthError::InvalidName)?;
    match fs::mkdir(fs_caps, fs_slot, ROOT_NODE, home_str) {
        Ok(_) | Err(FsError::AlreadyExists) => {}
        Err(e) => return Err(AuthError::from(e)),
    }
    store().add(name, password, home_str, salt)
}

/// Autentica credenciales. Devuelve error genérico si el usuario no existe o la
/// contraseña es incorrecta (no revela existencia).
pub fn login(
    name: &str,
    password: &str,
    fs_caps_for_home: &CapTable,
    fs_slot: usize,
) -> Result<Session, AuthError> {
    let nb = name.as_bytes();
    // Hash ficticio para igualar trabajo cuando el usuario no existe.
    let dummy_hash = temp_hash(password.as_bytes(), 0xdead_beef_cafe_babe);
    let (found_hash, found_salt, user_copy) = match store().find(nb) {
        Some(idx) => {
            let u = &store().users[idx];
            (u.hash, u.salt, Some(*u))
        }
        None => (dummy_hash, 0xdead_beef_cafe_babe, None),
    };
    let calc = temp_hash(password.as_bytes(), found_salt);
    let ok = eq_const(&calc, &found_hash);
    let u = match user_copy {
        Some(u) if ok => u,
        _ => return Err(AuthError::InvalidCredentials),
    };
    // Resolver directorio personal
    fs::ensure_init();
    let home_path = core::str::from_utf8(u.home_str_bytes()).map_err(|_| AuthError::Fs)?;
    let cwd = fs::cd(fs_caps_for_home, fs_slot, ROOT_NODE, home_path).map_err(AuthError::from)?;
    // Construir tabla de capacidades de la sesión: RW sobre el FS.
    let mut caps = CapTable::new();
    caps.insert(
        SESSION_FS_SLOT,
        Capability {
            object: FS_OBJECT_ID,
            rights: Rights::READ.union(Rights::WRITE),
        },
    )
    .map_err(|_| AuthError::Fs)?;
    let mut session = Session {
        username: [0; MAX_NAME],
        username_len: u.name_len,
        groups: u.groups,
        caps,
        home: u.home,
        home_len: u.home_len,
        cwd,
    };
    let mut i = 0usize;
    while i < u.name_len as usize { session.username[i] = u.name[i]; i += 1; }
    Ok(session)
}

/// Dibuja la pantalla de login en la consola (frontend textual).
pub fn draw_login_screen(console: &mut crate::console::Console) {
    use core::fmt::Write;
    console.set_foreground(crate::console::ACCENT);
    let _ = writeln!(console, "");
    let _ = writeln!(console, "=== Meridian login ===");
    console.set_foreground(crate::console::TEXT);
    let _ = writeln!(console, "usuario: _");
    let _ = writeln!(console, "contrasena: _");
    let _ = writeln!(console, "(alpha: hash temporal, no apto para produccion)");
}
