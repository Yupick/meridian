//! Auto-pruebas del servicio de archivos RAM (A7).
//!
//! Cubre cada operación (mkdir, rmdir vacío, rm, listar, leer, escribir, cd),
//! el error al borrar un directorio no vacío y la denegación de escritura con
//! una capacidad de solo lectura.

use core::fmt::Write;

use crate::cap::{CapError, CapTable, Capability, Rights};
use crate::console::Console;

use super::{self as fs, DirEntry, FsError, NodeType, FS_OBJECT_ID, MAX_LIST_ENTRIES};

const SLOT_RW: usize = 0;
const SLOT_RO: usize = 1;

/// Ejecuta la prueba A7 completa.
pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(msg) => {
            let _ = writeln!(console, "A7: fs service tests FAIL: {msg}");
            false
        }
    }
}

fn run_inner(console: &mut Console) -> Result<(), &'static str> {
    fs::init();

    let mut rw_caps = CapTable::new();
    rw_caps
        .insert(
            SLOT_RW,
            Capability {
                object: FS_OBJECT_ID,
                rights: Rights::READ.union(Rights::WRITE),
            },
        )
        .map_err(|_| "no se pudo crear capacidad RW")?;

    let mut ro_caps = CapTable::new();
    ro_caps
        .insert(
            SLOT_RO,
            Capability {
                object: FS_OBJECT_ID,
                rights: Rights::READ,
            },
        )
        .map_err(|_| "no se pudo crear capacidad RO")?;

    let mut cwd = fs::root_dir();

    // mkdir
    fs::mkdir(&rw_caps, SLOT_RW, cwd, "docs").map_err(|_| "mkdir docs falló")?;
    fs::mkdir(&rw_caps, SLOT_RW, cwd, "tmp").map_err(|_| "mkdir tmp falló")?;

    // cd
    cwd = fs::cd(&rw_caps, SLOT_RW, cwd, "docs").map_err(|_| "cd docs falló")?;
    if cwd == fs::root_dir() {
        return Err("cd no cambió el directorio actual");
    }

    // escribir (crea archivo) + leer
    let payload = b"hola-meridian";
    let written = fs::write(&rw_caps, SLOT_RW, cwd, "nota.txt", payload)
        .map_err(|_| "write nota.txt falló")?;
    if written != payload.len() {
        return Err("write devolvió tamaño incorrecto");
    }
    let mut buf = [0u8; 64];
    let n = fs::read(&rw_caps, SLOT_RW, cwd, "nota.txt", &mut buf)
        .map_err(|_| "read nota.txt falló")?;
    if n != payload.len() || &buf[..n] != payload {
        return Err("read devolvió contenido incorrecto");
    }

    // listar (atributos tipados: nombre, tipo, tamaño)
    let mut entries = [DirEntry {
        attr: fs::NodeAttr {
            name: [0; fs::MAX_NAME_LEN],
            name_len: 0,
            node_type: NodeType::File,
            size: 0,
        },
    }; MAX_LIST_ENTRIES];
    let count = fs::ls(&rw_caps, SLOT_RW, cwd, ".", &mut entries)
        .map_err(|_| "ls en docs falló")?;
    if count != 1 {
        return Err("ls no devolvió 1 entrada en docs");
    }
    let entry = &entries[0];
    if entry.attr.name_bytes() != b"nota.txt"
        || entry.attr.node_type != NodeType::File
        || entry.attr.size != payload.len() as u32
    {
        return Err("ls devolvió atributos tipados incorrectos");
    }

    // volver a raíz y verificar listado de directorios
    cwd = fs::cd(&rw_caps, SLOT_RW, cwd, "/").map_err(|_| "cd / falló")?;
    let count_root = fs::ls(&rw_caps, SLOT_RW, cwd, "/", &mut entries)
        .map_err(|_| "ls en raíz falló")?;
    if count_root != 2 {
        return Err("ls en raíz no devolvió 2 entradas");
    }
    let mut seen_docs = false;
    let mut seen_tmp = false;
    let mut i = 0usize;
    while i < count_root {
        let name = entries[i].attr.name_bytes();
        if name == b"docs" && entries[i].attr.node_type == NodeType::Directory {
            seen_docs = true;
        }
        if name == b"tmp" && entries[i].attr.node_type == NodeType::Directory {
            seen_tmp = true;
        }
        i += 1;
    }
    if !seen_docs || !seen_tmp {
        return Err("ls en raíz no mostró docs/tmp como directorios");
    }

    // rm
    fs::rm(&rw_caps, SLOT_RW, cwd, "docs/nota.txt").map_err(|_| "rm nota.txt falló")?;
    if fs::read(&rw_caps, SLOT_RW, cwd, "docs/nota.txt", &mut buf) != Err(FsError::NotFound) {
        return Err("rm no eliminó el archivo");
    }

    // rmdir vacío
    fs::rmdir(&rw_caps, SLOT_RW, cwd, "tmp").map_err(|_| "rmdir tmp vacío falló")?;
    if fs::cd(&rw_caps, SLOT_RW, cwd, "tmp") != Err(FsError::NotFound) {
        return Err("rmdir no eliminó el directorio vacío");
    }

    // error al borrar directorio no vacío
    fs::write(&rw_caps, SLOT_RW, cwd, "docs/otro.txt", b"x")
        .map_err(|_| "write docs/otro.txt falló")?;
    match fs::rmdir(&rw_caps, SLOT_RW, cwd, "docs") {
        Err(FsError::DirectoryNotEmpty) => {}
        Err(_) => return Err("rmdir de dir no vacío devolvió error incorrecto"),
        Ok(()) => return Err("rmdir permitió borrar un directorio no vacío"),
    }

    let _ = writeln!(
        console,
        "A7: fs operations tests PASS (mkdir, rmdir-vacio, rm, listar, leer, escribir, cd; rmdir-no-vacio -> DirectoryNotEmpty)"
    );

    // capacidad de solo lectura no permite escribir (ni modificar)
    let ro_write = fs::write(&ro_caps, SLOT_RO, cwd, "docs/ro.txt", b"no");
    let ro_mkdir = fs::mkdir(&ro_caps, SLOT_RO, cwd, "docs/sub");
    let ro_rm = fs::rm(&ro_caps, SLOT_RO, cwd, "docs/otro.txt");
    let ro_rmdir = fs::rmdir(&ro_caps, SLOT_RO, cwd, "docs");

    if ro_write != Err(FsError::Capability(CapError::RightsDenied)) {
        return Err("capacidad read-only permitió escribir");
    }
    if ro_mkdir != Err(FsError::Capability(CapError::RightsDenied))
        || ro_rm != Err(FsError::Capability(CapError::RightsDenied))
        || ro_rmdir != Err(FsError::Capability(CapError::RightsDenied))
    {
        return Err("capacidad read-only permitió modificar el sistema de archivos");
    }

    // pero sí puede leer/listar/cd
    let mut ro_buf = [0u8; 8];
    let rn = fs::read(&ro_caps, SLOT_RO, cwd, "docs/otro.txt", &mut ro_buf)
        .map_err(|_| "capacidad read-only no pudo leer")?;
    if rn != 1 || ro_buf[0] != b'x' {
        return Err("lectura con capacidad read-only devolvió datos incorrectos");
    }
    let _ = fs::ls(&ro_caps, SLOT_RO, cwd, "docs", &mut entries)
        .map_err(|_| "capacidad read-only no pudo listar")?;
    let _ = fs::cd(&ro_caps, SLOT_RO, cwd, "docs")
        .map_err(|_| "capacidad read-only no pudo hacer cd")?;

    let _ = writeln!(
        console,
        "A7: read-only capability denied write PASS (write/mkdir/rm/rmdir -> RightsDenied; read/ls/cd permitidos)"
    );

    let _ = writeln!(
        console,
        "A7: fs service tests PASS (ops completas, atributos tipados, rmdir-no-vacio controlado, read-only sin escritura)"
    );
    Ok(())
}
