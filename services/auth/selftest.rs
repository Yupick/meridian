//! Auto-pruebas del servicio de login y usuarios (A8).

use core::fmt::Write;

use crate::cap::{CapTable, Capability, Rights};
use crate::console::Console;
use crate::fs::{self, FsError, FS_OBJECT_ID, SESSION_FS_SLOT as FS_SLOT};

use super as auth;

pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(msg) => {
            let _ = writeln!(console, "A8: login/users tests FAIL: {msg}");
            false
        }
    }
}

fn run_inner(console: &mut Console) -> Result<(), &'static str> {
    // Pantalla de login (frontend textual)
    auth::draw_login_screen(console);
    let _ = writeln!(console, "A8: login screen shown");

    // FS limpio y capacidad administrativa para preparar usuarios/homes.
    fs::init();
    auth::reset_users();
    let mut admin_caps = CapTable::new();
    admin_caps
        .insert(FS_SLOT, Capability { object: FS_OBJECT_ID, rights: Rights::READ.union(Rights::WRITE) })
        .map_err(|_| "no se pudo crear cap admin FS")?;

    auth::register_user(&admin_caps, FS_SLOT, "root", "meridian", 0xA801)
        .map_err(|_| "no se pudo registrar usuario root")?;
    auth::register_user(&admin_caps, FS_SLOT, "alice", "secret", 0xA802)
        .map_err(|_| "no se pudo registrar usuario alice")?;

    // Login correcto abre sesión.
    let session = auth::login("alice", "secret", &admin_caps, FS_SLOT)
        .map_err(|_| "login correcto fue rechazado")?;
    if session.username_bytes() != b"alice" {
        return Err("sesión con usuario incorrecto");
    }
    if session.home_bytes() != b"/home/alice" {
        return Err("sesión con home incorrecto");
    }
    let _ = writeln!(console, "A8: login correcto abre sesion (user=alice, home=/home/alice)");

    // Contraseña incorrecta se rechaza sin revelar si el usuario existe.
    let err_wrong = auth::login("alice", "badpass", &admin_caps, FS_SLOT).err();
    let err_unknown = auth::login("nobody", "badpass", &admin_caps, FS_SLOT).err();
    if err_wrong != Some(auth::AuthError::InvalidCredentials) {
        return Err("contraseña incorrecta no devolvió error genérico");
    }
    if err_unknown != Some(auth::AuthError::InvalidCredentials) {
        return Err("usuario inexistente no devolvió el mismo error genérico");
    }
    let _ = writeln!(
        console,
        "A8: invalid credentials rejected without user disclosure (wrong-pass == unknown-user == InvalidCredentials)"
    );

    // Tras login, el usuario tiene capacidades sobre su directorio personal.
    // Escribir y leer dentro de /home/alice usando las capacidades de la sesión.
    let msg = b"perfil";
    fs::write(&session.caps, FS_SLOT, session.cwd, "perfil.txt", msg)
        .map_err(|_| "sesión no pudo escribir en su directorio personal")?;
    let mut buf = [0u8; 16];
    let n = fs::read(&session.caps, FS_SLOT, session.cwd, "perfil.txt", &mut buf)
        .map_err(|_| "sesión no pudo leer su archivo personal")?;
    if n != msg.len() || &buf[..n] != msg {
        return Err("contenido leído en home incorrecto");
    }
    // Crear subdirectorio en home.
    fs::mkdir(&session.caps, FS_SLOT, session.cwd, "docs")
        .map_err(|_| "sesión no pudo crear directorio en home")?;
    fs::write(&session.caps, FS_SLOT, session.cwd, "docs/nota.txt", b"ok")
        .map_err(|_| "sesión no pudo escribir en subdirectorio de home")?;

    // Una capacidad sin WRITE no permite escribir en home (control de capacidades).
    let mut ro = CapTable::new();
    ro.insert(FS_SLOT, Capability { object: FS_OBJECT_ID, rights: Rights::READ })
        .map_err(|_| "no se pudo crear cap RO")?;
    match fs::write(&ro, FS_SLOT, session.cwd, "hack.txt", b"x") {
        Err(FsError::Capability(crate::cap::CapError::RightsDenied)) => {}
        _ => return Err("capacidad sin WRITE permitió escribir en home"),
    }

    let _ = writeln!(
        console,
        "A8: session has capabilities over personal directory (RW en /home/alice: write/read/mkdir ok; cap sin WRITE denegada)"
    );
    let _ = writeln!(
        console,
        "A8: login/users tests PASS (login ok, rechazo generico, capacidades sobre home)"
    );
    Ok(())
}
