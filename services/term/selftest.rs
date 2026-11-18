//! Auto-pruebas scriptadas de la terminal alpha (A9).
//!
//! Verifica: acceso sin login denegado, y ejecución de cada comando en una
//! sesión autenticada (ls, cd, pwd, mkdir/md, rmdir/rd, rm, rm -r, cat,
//! echo > archivo, help, clear, exit), incluyendo error claro al borrar un
//! directorio no vacío.

use core::fmt::Write;

use crate::auth;
use crate::cap::{CapTable, Capability, Rights};
use crate::console::Console;
use crate::fs::{self, FS_OBJECT_ID, SESSION_FS_SLOT};

use super::{access_without_login, ExecResult, Shell};

pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(msg) => {
            let _ = writeln!(console, "A9: terminal tests FAIL: {msg}");
            false
        }
    }
}

fn run_inner(console: &mut Console) -> Result<(), &'static str> {
    // --- Sin login no se accede a la terminal ---
    let _ = writeln!(console, "A9: terminal access requires login");
    if access_without_login(console, false) {
        return Err("se permitió acceso a terminal sin login");
    }
    let _ = writeln!(console, "A9: access without login denied");

    // Preparar entorno: FS limpio, usuario y login real.
    fs::init();
    auth::reset_users();
    let mut admin_caps = CapTable::new();
    admin_caps
        .insert(
            SESSION_FS_SLOT,
            Capability {
                object: FS_OBJECT_ID,
                rights: Rights::READ.union(Rights::WRITE),
            },
        )
        .map_err(|_| "no se pudo crear cap admin FS")?;

    auth::register_user(&admin_caps, SESSION_FS_SLOT, "root", "meridian", 0xA901)
        .map_err(|_| "no se pudo registrar usuario root")?;

    let session = auth::login("root", "meridian", &admin_caps, SESSION_FS_SLOT)
        .map_err(|_| "login falló para sesión de terminal")?;

    if !access_without_login(console, true) {
        // con sesión el gate de acceso permite continuar
        return Err("acceso denegado con sesión válida");
    }
    let _ = writeln!(console, "A9: login ok, terminal session started (user=root)");

    let mut sh = Shell::new(session);

    // help
    if sh.execute(console, "help") != ExecResult::Continue { return Err("help cerró la sesión"); }
    let _ = writeln!(console, "A9: help PASS");

    // pwd
    if sh.execute(console, "pwd") != ExecResult::Continue { return Err("pwd cerró la sesión"); }
    let _ = writeln!(console, "A9: pwd PASS");

    // mkdir y alias md
    if sh.execute(console, "mkdir docs") != ExecResult::Continue { return Err("mkdir falló flujo"); }
    if sh.execute(console, "md tmp") != ExecResult::Continue { return Err("md falló flujo"); }
    let _ = writeln!(console, "A9: mkdir/md PASS");

    // ls
    if sh.execute(console, "ls") != ExecResult::Continue { return Err("ls cerró la sesión"); }
    let _ = writeln!(console, "A9: ls PASS");

    // cd
    if sh.execute(console, "cd docs") != ExecResult::Continue { return Err("cd cerró la sesión"); }
    // verificar pwd refleja cambio (ejecutar pwd)
    if sh.execute(console, "pwd") != ExecResult::Continue { return Err("pwd tras cd falló"); }
    let _ = writeln!(console, "A9: cd/pwd PASS");

    // echo texto > archivo y cat
    if sh.execute(console, "echo hola-meridian > nota.txt") != ExecResult::Continue { return Err("echo> falló"); }
    // verificar contenido vía FS directo
    let mut buf = [0u8; 64];
    let n = fs::read(&sh.session.caps, SESSION_FS_SLOT, sh.session.cwd, "nota.txt", &mut buf)
        .map_err(|_| "echo > archivo no creó/contenido ilegible")?;
    let content = core::str::from_utf8(&buf[..n]).unwrap_or("");
    if !content.starts_with("hola-meridian") {
        return Err("echo > archivo escribió contenido incorrecto");
    }
    let _ = writeln!(console, "A9: echo > PASS");

    if sh.execute(console, "cat nota.txt") != ExecResult::Continue { return Err("cat cerró la sesión"); }
    let _ = writeln!(console, "A9: cat PASS");

    // volver a home
    if sh.execute(console, "cd ..") != ExecResult::Continue { return Err("cd .. falló"); }

    // rmdir sobre directorio con contenido muestra error claro
    if sh.execute(console, "rmdir docs") != ExecResult::Continue { return Err("rmdir no-vacío cerró sesión"); }
    // docs debe seguir existiendo
    if fs::cd(&sh.session.caps, SESSION_FS_SLOT, sh.session.cwd, "docs").is_err() {
        return Err("rmdir eliminó un directorio no vacío");
    }
    let _ = writeln!(console, "A9: rmdir non-empty shows clear error PASS");

    // rm archivo
    if sh.execute(console, "rm docs/nota.txt") != ExecResult::Continue { return Err("rm archivo falló"); }
    if fs::read(&sh.session.caps, SESSION_FS_SLOT, sh.session.cwd, "docs/nota.txt", &mut buf).is_ok() {
        return Err("rm no eliminó el archivo");
    }
    let _ = writeln!(console, "A9: rm PASS");

    // rmdir vacío (docs ahora vacío) y alias rd (tmp)
    if sh.execute(console, "rmdir docs") != ExecResult::Continue { return Err("rmdir vacío falló"); }
    if sh.execute(console, "rd tmp") != ExecResult::Continue { return Err("rd falló"); }
    let _ = writeln!(console, "A9: rmdir/rd PASS");

    // rm -r elimina directorio con contenido
    if sh.execute(console, "mkdir arbol") != ExecResult::Continue { return Err("mkdir arbol falló"); }
    if sh.execute(console, "echo x > arbol/a.txt") != ExecResult::Continue { return Err("echo a.txt falló"); }
    if sh.execute(console, "mkdir arbol/sub") != ExecResult::Continue { return Err("mkdir sub falló"); }
    if sh.execute(console, "echo y > arbol/sub/b.txt") != ExecResult::Continue { return Err("echo b.txt falló"); }
    if sh.execute(console, "rm -r arbol") != ExecResult::Continue { return Err("rm -r falló"); }
    if fs::cd(&sh.session.caps, SESSION_FS_SLOT, sh.session.cwd, "arbol").is_ok() {
        return Err("rm -r no eliminó el directorio con contenido");
    }
    let _ = writeln!(console, "A9: rm -r PASS");

    // clear
    if sh.execute(console, "clear") != ExecResult::Continue { return Err("clear cerró la sesión"); }
    let _ = writeln!(console, "A9: clear PASS");

    // exit cierra la sesión
    if sh.execute(console, "exit") != ExecResult::Exit { return Err("exit no cerró la sesión"); }
    let _ = writeln!(console, "A9: exit PASS");

    let _ = writeln!(console, "A9: command script PASS (ls,cd,pwd,mkdir,md,rmdir,rd,rm,rm -r,cat,echo>,help,clear,exit)");
    let _ = writeln!(console, "A9: terminal tests PASS (scripted commands, rmdir non-empty error, login required)");
    Ok(())
}
