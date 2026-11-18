//! Servicio de sistema de archivos mínimo en memoria RAM (A7).
//!
//! Objetos tipados (archivo, directorio) con nombre, tipo y tamaño, almacenados
//! en tablas de tamaño fijo (sin heap). Las operaciones exigen una capacidad
//! válida sobre el objeto del sistema de archivos ([`FS_OBJECT_ID`]) con el
//! derecho adecuado: [`Rights::READ`] para consultar/navegar y
//! [`Rights::WRITE`] para modificar.
//!
//! Un proceso con capacidad de solo lectura puede listar/leer/cd pero ninguna
//! operación que altere el contenido o la estructura (escribir, mkdir, rm,
//! rmdir).

use core::cell::UnsafeCell;
use core::fmt;

use crate::cap::{CapError, CapTable, ObjectId, Rights};

pub mod selftest;

/// Slot de capacidad recomendado para el FS en sesiones.
pub const SESSION_FS_SLOT: usize = 0;

/// Objeto de capacidad que representa el servicio de archivos RAM.
pub const FS_OBJECT_ID: ObjectId = 0x0100;

/// Identificador interno de nodo.
pub type NodeId = u16;

/// Nodo inválido / ausente.
pub const INVALID_NODE: NodeId = u16::MAX;

/// Nodo raíz (`/`).
pub const ROOT_NODE: NodeId = 0;

/// Límites de almacenamiento en RAM.
pub const MAX_NODES: usize = 32;
pub const MAX_NAME_LEN: usize = 24;
pub const MAX_FILE_SIZE: usize = 256;
pub const MAX_DIR_ENTRIES: usize = 12;
pub const MAX_LIST_ENTRIES: usize = MAX_DIR_ENTRIES;

/// Tipo de objeto del sistema de archivos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeType {
    File,
    Directory,
}

impl fmt::Display for NodeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => write!(f, "file"),
            Self::Directory => write!(f, "dir"),
        }
    }
}

/// Atributos tipados de un objeto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeAttr {
    pub name: [u8; MAX_NAME_LEN],
    pub name_len: u8,
    pub node_type: NodeType,
    pub size: u32,
}

impl NodeAttr {
    pub fn name_bytes(&self) -> &[u8] {
        &self.name[..self.name_len as usize]
    }
}

/// Entrada devuelta por `ls`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub attr: NodeAttr,
}

/// Errores del servicio de archivos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    Capability(CapError),
    /// La capacidad no referencia al objeto FS.
    WrongObject,
    NotFound,
    AlreadyExists,
    NotADirectory,
    NotAFile,
    DirectoryNotEmpty,
    InvalidName,
    NameTooLong,
    PathTooLong,
    NoSpace,
    FileTooLarge,
    IsRoot,
    BufferTooSmall,
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(e) => write!(f, "capacidad: {e}"),
            Self::WrongObject => write!(f, "capacidad no corresponde al sistema de archivos"),
            Self::NotFound => write!(f, "no encontrado"),
            Self::AlreadyExists => write!(f, "ya existe"),
            Self::NotADirectory => write!(f, "no es un directorio"),
            Self::NotAFile => write!(f, "no es un archivo"),
            Self::DirectoryNotEmpty => write!(f, "directorio no vacío"),
            Self::InvalidName => write!(f, "nombre inválido"),
            Self::NameTooLong => write!(f, "nombre demasiado largo"),
            Self::PathTooLong => write!(f, "ruta demasiado larga"),
            Self::NoSpace => write!(f, "sin espacio"),
            Self::FileTooLarge => write!(f, "archivo demasiado grande"),
            Self::IsRoot => write!(f, "operación inválida sobre la raíz"),
            Self::BufferTooSmall => write!(f, "búfer insuficiente"),
        }
    }
}

impl From<CapError> for FsError {
    fn from(e: CapError) -> Self {
        Self::Capability(e)
    }
}

#[derive(Clone, Copy)]
struct Node {
    in_use: bool,
    node_type: NodeType,
    parent: NodeId,
    name: [u8; MAX_NAME_LEN],
    name_len: u8,
    size: u32,
    // Archivo: contenido en `data[..size]`.
    data: [u8; MAX_FILE_SIZE],
    // Directorio: hijos en `children[..child_count]`.
    children: [NodeId; MAX_DIR_ENTRIES],
    child_count: u8,
}

impl Node {
    const fn empty() -> Self {
        Self {
            in_use: false,
            node_type: NodeType::File,
            parent: INVALID_NODE,
            name: [0; MAX_NAME_LEN],
            name_len: 0,
            size: 0,
            data: [0; MAX_FILE_SIZE],
            children: [INVALID_NODE; MAX_DIR_ENTRIES],
            child_count: 0,
        }
    }

    fn attr(&self) -> NodeAttr {
        NodeAttr {
            name: self.name,
            name_len: self.name_len,
            node_type: self.node_type,
            size: self.size,
        }
    }

    fn name_eq(&self, name: &[u8]) -> bool {
        self.in_use && self.name_len as usize == name.len() && &self.name[..name.len()] == name
    }
}

struct Fs {
    nodes: [Node; MAX_NODES],
    initialized: bool,
}

impl Fs {
    const fn new() -> Self {
        Self {
            nodes: [Node::empty(); MAX_NODES],
            initialized: false,
        }
    }

    fn init(&mut self) {
        let mut i = 0;
        while i < MAX_NODES {
            self.nodes[i] = Node::empty();
            i += 1;
        }
        // Raíz
        let root = &mut self.nodes[ROOT_NODE as usize];
        root.in_use = true;
        root.node_type = NodeType::Directory;
        root.parent = ROOT_NODE;
        root.name_len = 0;
        root.size = 0;
        root.child_count = 0;
        self.initialized = true;
    }

    fn ensure_init(&mut self) {
        if !self.initialized {
            self.init();
        }
    }

    fn alloc_node(&mut self) -> Result<NodeId, FsError> {
        let mut i = 1; // 0 reservado para raíz
        while i < MAX_NODES {
            if !self.nodes[i].in_use {
                self.nodes[i] = Node::empty();
                self.nodes[i].in_use = true;
                return Ok(i as NodeId);
            }
            i += 1;
        }
        Err(FsError::NoSpace)
    }

    fn get(&self, id: NodeId) -> Result<&Node, FsError> {
        let idx = id as usize;
        if idx >= MAX_NODES {
            return Err(FsError::NotFound);
        }
        let n = &self.nodes[idx];
        if n.in_use { Ok(n) } else { Err(FsError::NotFound) }
    }

    fn get_mut(&mut self, id: NodeId) -> Result<&mut Node, FsError> {
        let idx = id as usize;
        if idx >= MAX_NODES {
            return Err(FsError::NotFound);
        }
        let n = &mut self.nodes[idx];
        if n.in_use { Ok(n) } else { Err(FsError::NotFound) }
    }

    fn find_child(&self, dir: NodeId, name: &[u8]) -> Option<NodeId> {
        let d = match self.get(dir) { Ok(n) => n, Err(_) => return None };
        if d.node_type != NodeType::Directory { return None; }
        let mut i = 0usize;
        while i < d.child_count as usize {
            let cid = d.children[i];
            if let Ok(child) = self.get(cid) {
                if child.name_eq(name) { return Some(cid); }
            }
            i += 1;
        }
        None
    }

    fn resolve(&self, cwd: NodeId, path: &[u8]) -> Result<NodeId, FsError> {
        if path.is_empty() || (path.len() == 1 && path[0] == b'.') {
            // "." o vacío -> cwd
            self.get(cwd)?;
            return Ok(cwd);
        }
        if path.len() == 1 && path[0] == b'/' {
            return Ok(ROOT_NODE);
        }
        let mut current = if !path.is_empty() && path[0] == b'/' { ROOT_NODE } else { cwd };
        self.get(current)?; // validar cwd
        let bytes = path;
        let mut i = 0usize;
        let len = bytes.len();
        // saltar '/' inicial
        if i < len && bytes[i] == b'/' { i += 1; }
        while i < len {
            // saltar separadores repetidos
            while i < len && bytes[i] == b'/' { i += 1; }
            if i >= len { break; }
            let start = i;
            while i < len && bytes[i] != b'/' { i += 1; }
            let comp = &bytes[start..i];
            if comp.is_empty() || comp == b"." {
                // nada
            } else if comp == b".." {
                let node = self.get(current)?;
                if node.node_type != NodeType::Directory { return Err(FsError::NotADirectory); }
                current = node.parent;
            } else {
                let node = self.get(current)?;
                if node.node_type != NodeType::Directory { return Err(FsError::NotADirectory); }
                match self.find_child(current, comp) {
                    Some(id) => current = id,
                    None => return Err(FsError::NotFound),
                }
            }
        }
        Ok(current)
    }

    fn split_parent_name<'a>(&self, cwd: NodeId, path: &'a [u8]) -> Result<(NodeId, &'a [u8]), FsError> {
        if path.is_empty() { return Err(FsError::InvalidName); }
        // recortar '/' final (excepto raíz sola)
        let mut end = path.len();
        while end > 1 && path[end - 1] == b'/' { end -= 1; }
        let p = &path[..end];
        if p.len() == 1 && p[0] == b'/' { return Err(FsError::IsRoot); }
        // encontrar último '/'
        let mut sep = None;
        let mut j = p.len();
        while j > 0 {
            j -= 1;
            if p[j] == b'/' { sep = Some(j); break; }
        }
        let (parent_path, name) = match sep {
            Some(0) => (&b"/"[..], &p[1..]),
            Some(idx) => (&p[..idx], &p[idx + 1..]),
            None => (&b""[..], p),
        };
        if name.is_empty() || name == b"." || name == b".." { return Err(FsError::InvalidName); }
        if name.len() > MAX_NAME_LEN { return Err(FsError::NameTooLong); }
        let parent = if parent_path.is_empty() { cwd } else { self.resolve(cwd, parent_path)? };
        Ok((parent, name))
    }
}

struct GlobalFs(UnsafeCell<Fs>);

// SAFETY: kernel mono-CPU, sin interrupciones/reentrancia durante las operaciones
// (mismo modelo que mm::buddy e ipc).
unsafe impl Sync for GlobalFs {}

static FS: GlobalFs = GlobalFs(UnsafeCell::new(Fs::new()));

fn with_fs<R>(f: impl FnOnce(&mut Fs) -> R) -> R {
    // SAFETY: ver GlobalFs.
    unsafe { f(&mut *FS.0.get()) }
}

/// Inicializa/reinicia el sistema de archivos en RAM con solo la raíz.
pub fn init() {
    with_fs(|fs| fs.init());
}

/// Asegura inicialización (idempotente).
pub fn ensure_init() {
    with_fs(|fs| fs.ensure_init());
}

/// Directorio raíz.
pub fn root_dir() -> NodeId { ROOT_NODE }

fn check_cap(caps: &CapTable, slot: usize, required: Rights) -> Result<(), FsError> {
    let cap = caps.check(slot, required)?;
    if cap.object != FS_OBJECT_ID {
        return Err(FsError::WrongObject);
    }
    Ok(())
}

fn copy_name(dst: &mut [u8; MAX_NAME_LEN], name: &[u8]) -> u8 {
    let mut i = 0;
    while i < name.len() && i < MAX_NAME_LEN {
        dst[i] = name[i];
        i += 1;
    }
    i as u8
}

fn path_bytes(path: &str) -> &[u8] { path.as_bytes() }

/// Crea un directorio en `path` (relativo a `cwd` o absoluto).
pub fn mkdir(caps: &CapTable, slot: usize, cwd: NodeId, path: &str) -> Result<NodeId, FsError> {
    with_fs(|fs| {
        fs.ensure_init();
        check_cap(caps, slot, Rights::WRITE)?;
        let pb = path_bytes(path);
        let (parent_id, name) = fs.split_parent_name(cwd, pb)?;
        // padre debe ser dir
        match fs.get(parent_id)?.node_type { NodeType::Directory => {}, _ => return Err(FsError::NotADirectory) }
        if fs.find_child(parent_id, name).is_some() { return Err(FsError::AlreadyExists); }
        // espacio en padre
        if fs.get(parent_id)?.child_count as usize >= MAX_DIR_ENTRIES { return Err(FsError::NoSpace); }
        let new_id = fs.alloc_node()?;
        // configurar nuevo nodo
        {
            let node = fs.get_mut(new_id)?;
            node.node_type = NodeType::Directory;
            node.parent = parent_id;
            node.name_len = copy_name(&mut node.name, name);
            node.size = 0;
            node.child_count = 0;
        }
        // enlazar en padre
        let parent = fs.get_mut(parent_id)?;
        let idx = parent.child_count as usize;
        parent.children[idx] = new_id;
        parent.child_count += 1;
        Ok(new_id)
    })
}

/// Elimina un directorio vacío.
pub fn rmdir(caps: &CapTable, slot: usize, cwd: NodeId, path: &str) -> Result<(), FsError> {
    with_fs(|fs| {
        fs.ensure_init();
        check_cap(caps, slot, Rights::WRITE)?;
        let target = fs.resolve(cwd, path_bytes(path))?;
        if target == ROOT_NODE { return Err(FsError::IsRoot); }
        {
            let node = fs.get(target)?;
            match node.node_type { NodeType::Directory => {}, _ => return Err(FsError::NotADirectory) }
            if node.child_count != 0 { return Err(FsError::DirectoryNotEmpty); }
        }
        let parent_id = fs.get(target)?.parent;
        // quitar de padre
        let parent = fs.get_mut(parent_id)?;
        let mut i = 0usize;
        let mut found = false;
        while i < parent.child_count as usize {
            if parent.children[i] == target {
                // compactar
                let mut j = i;
                while j + 1 < parent.child_count as usize {
                    parent.children[j] = parent.children[j + 1];
                    j += 1;
                }
                parent.child_count -= 1;
                parent.children[parent.child_count as usize] = INVALID_NODE;
                found = true;
                break;
            }
            i += 1;
        }
        if !found { return Err(FsError::NotFound); }
        // liberar nodo
        let node = fs.get_mut(target)?;
        node.in_use = false;
        Ok(())
    })
}

/// Elimina un archivo.
pub fn rm(caps: &CapTable, slot: usize, cwd: NodeId, path: &str) -> Result<(), FsError> {
    with_fs(|fs| {
        fs.ensure_init();
        check_cap(caps, slot, Rights::WRITE)?;
        let target = fs.resolve(cwd, path_bytes(path))?;
        if target == ROOT_NODE { return Err(FsError::IsRoot); }
        {
            let node = fs.get(target)?;
            match node.node_type { NodeType::File => {}, _ => return Err(FsError::NotAFile) }
        }
        let parent_id = fs.get(target)?.parent;
        let parent = fs.get_mut(parent_id)?;
        let mut i = 0usize;
        let mut found = false;
        while i < parent.child_count as usize {
            if parent.children[i] == target {
                let mut j = i;
                while j + 1 < parent.child_count as usize {
                    parent.children[j] = parent.children[j + 1];
                    j += 1;
                }
                parent.child_count -= 1;
                parent.children[parent.child_count as usize] = INVALID_NODE;
                found = true;
                break;
            }
            i += 1;
        }
        if !found { return Err(FsError::NotFound); }
        let node = fs.get_mut(target)?;
        node.in_use = false;
        Ok(())
    })
}

/// Escribe `data` en el archivo `path` (crea si no existe, trunca si existe).
pub fn write(caps: &CapTable, slot: usize, cwd: NodeId, path: &str, data: &[u8]) -> Result<usize, FsError> {
    with_fs(|fs| {
        fs.ensure_init();
        check_cap(caps, slot, Rights::WRITE)?;
        if data.len() > MAX_FILE_SIZE { return Err(FsError::FileTooLarge); }
        let pb = path_bytes(path);
        // intentar resolver existente
        match fs.resolve(cwd, pb) {
            Ok(target) => {
                {
                    let node = fs.get(target)?;
                    if node.node_type != NodeType::File { return Err(FsError::NotAFile); }
                }
                let node = fs.get_mut(target)?;
                let mut i = 0usize;
                while i < data.len() { node.data[i] = data[i]; i += 1; }
                node.size = data.len() as u32;
                Ok(data.len())
            }
            Err(FsError::NotFound) => {
                // crear archivo
                let (parent_id, name) = fs.split_parent_name(cwd, pb)?;
                match fs.get(parent_id)?.node_type { NodeType::Directory => {}, _ => return Err(FsError::NotADirectory) }
                if fs.find_child(parent_id, name).is_some() { return Err(FsError::AlreadyExists); }
                if fs.get(parent_id)?.child_count as usize >= MAX_DIR_ENTRIES { return Err(FsError::NoSpace); }
                let new_id = fs.alloc_node()?;
                {
                    let node = fs.get_mut(new_id)?;
                    node.node_type = NodeType::File;
                    node.parent = parent_id;
                    node.name_len = copy_name(&mut node.name, name);
                    node.size = data.len() as u32;
                    let mut i = 0usize;
                    while i < data.len() { node.data[i] = data[i]; i += 1; }
                }
                let parent = fs.get_mut(parent_id)?;
                let idx = parent.child_count as usize;
                parent.children[idx] = new_id;
                parent.child_count += 1;
                Ok(data.len())
            }
            Err(e) => Err(e),
        }
    })
}

/// Lee el contenido de un archivo en `out`. Devuelve bytes leídos.
pub fn read(caps: &CapTable, slot: usize, cwd: NodeId, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
    with_fs(|fs| {
        fs.ensure_init();
        check_cap(caps, slot, Rights::READ)?;
        let target = fs.resolve(cwd, path_bytes(path))?;
        let node = fs.get(target)?;
        if node.node_type != NodeType::File { return Err(FsError::NotAFile); }
        let size = node.size as usize;
        if out.len() < size { return Err(FsError::BufferTooSmall); }
        let mut i = 0usize;
        while i < size { out[i] = node.data[i]; i += 1; }
        Ok(size)
    })
}

/// Obtiene atributos de un objeto.
pub fn stat(caps: &CapTable, slot: usize, cwd: NodeId, path: &str) -> Result<NodeAttr, FsError> {
    with_fs(|fs| {
        fs.ensure_init();
        check_cap(caps, slot, Rights::READ)?;
        let target = fs.resolve(cwd, path_bytes(path))?;
        Ok(fs.get(target)?.attr())
    })
}

/// Lista un directorio en `out[..count]`. Devuelve número de entradas.
pub fn ls(caps: &CapTable, slot: usize, cwd: NodeId, path: &str, out: &mut [DirEntry]) -> Result<usize, FsError> {
    with_fs(|fs| {
        fs.ensure_init();
        check_cap(caps, slot, Rights::READ)?;
        let target = fs.resolve(cwd, path_bytes(path))?;
        let node = fs.get(target)?;
        if node.node_type != NodeType::Directory { return Err(FsError::NotADirectory); }
        let count = node.child_count as usize;
        if out.len() < count { return Err(FsError::BufferTooSmall); }
        // copiar ids primero para evitar doble préstamo
        let mut ids = [INVALID_NODE; MAX_DIR_ENTRIES];
        let mut i = 0usize;
        while i < count { ids[i] = node.children[i]; i += 1; }
        i = 0;
        while i < count {
            let child = fs.get(ids[i])?;
            out[i] = DirEntry { attr: child.attr() };
            i += 1;
        }
        Ok(count)
    })
}

/// Cambia el directorio actual: resuelve `path` y devuelve el nuevo cwd.
pub fn cd(caps: &CapTable, slot: usize, cwd: NodeId, path: &str) -> Result<NodeId, FsError> {
    with_fs(|fs| {
        fs.ensure_init();
        check_cap(caps, slot, Rights::READ)?;
        let target = fs.resolve(cwd, path_bytes(path))?;
        let node = fs.get(target)?;
        if node.node_type != NodeType::Directory { return Err(FsError::NotADirectory); }
        Ok(target)
    })
}
