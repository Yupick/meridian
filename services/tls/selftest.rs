//! Auto-pruebas T2.3: acceso remoto cifrado (TLS 1.3 PSK sobre UDP).
//!
//! Prepara volumen cifrado y sesiones T2.2, ejecuta el bucle de red para
//! procesar el handshake/tráfico generado por `tools/net-peer.py` y emite los
//! 6 marcadores exigidos por QA.

use core::fmt::Write;

use crate::auth;
use crate::blk::{BlockDeviceId, BLOCK_OBJECT_ID, SESSION_BLOCK_SLOT};
use crate::cap::{CapTable, Capability, Rights};
use crate::console::Console;
use crate::fs;
use crate::net::{self, Progress};
use crate::perm::{PermissionedVolume, OBJ_PUBLIC_NOTICE, OBJ_SHARED_CONFIG};
use crate::vol::DEV_PASSPHRASE;

use super as tls;

const QA_DEVICE: BlockDeviceId = 1;
const POLL_ITERATIONS: u32 = 60_000_000;

struct Failure {
    step: &'static str,
    message: &'static str,
}

fn failure(step: &'static str, message: &'static str) -> Failure {
    Failure { step, message }
}

pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(failed) => {
            let _ = writeln!(console, "T2.3: {} FAIL: {}", failed.step, failed.message);
            false
        }
    }
}

fn ensure_user(fs_caps: &CapTable, name: &str, pass: &str, salt: u64) -> Result<(), Failure> {
    match auth::register_user(fs_caps, auth::SESSION_FS_SLOT, name, pass, salt) {
        Ok(()) => Ok(()),
        Err(auth::AuthError::NoSpace) => {
            // Puede existir ya (creado por A8/A9/T2.2); continuar.
            Ok(())
        }
        Err(_) => Err(failure("setup", "no se pudo registrar usuario")),
    }
}

fn run_inner(console: &mut Console) -> Result<(), Failure> {
    // Preparar FS y usuarios requeridos (root/alice/guest). Si ya existen por
    // pruebas previas, reutilizamos el registro.
    fs::ensure_init();
    // capacidad FS RW
    let mut fs_caps = CapTable::new();
    fs_caps
        .insert(
            auth::SESSION_FS_SLOT,
            Capability {
                object: fs::FS_OBJECT_ID,
                rights: Rights::READ.union(Rights::WRITE),
            },
        )
        .map_err(|_| failure("setup", "capacidad FS"))?;
    // intentar registrar; ignorar si ya estaban
    let _ = ensure_user(&fs_caps, "root", "meridian", 0xC301);
    let _ = ensure_user(&fs_caps, "alice", "secret", 0xC302);
    let _ = ensure_user(&fs_caps, "guest", "visitor", 0xC304);
    // bob no requerido pero útil coherencia
    let _ = ensure_user(&fs_caps, "bob", "builder", 0xC303);

    let root = auth::login("root", "meridian", &fs_caps, auth::SESSION_FS_SLOT)
        .map_err(|_| failure("setup", "login root"))?;
    let alice = auth::login("alice", "secret", &fs_caps, auth::SESSION_FS_SLOT)
        .map_err(|_| failure("setup", "login alice"))?;
    let guest = auth::login("guest", "visitor", &fs_caps, auth::SESSION_FS_SLOT)
        .map_err(|_| failure("setup", "login guest"))?;

    // Capacidad RW sobre bloques (cadena T1.2/T1.5/T2.2 intacta)
    let mut blk_caps = CapTable::new();
    blk_caps
        .insert(
            SESSION_BLOCK_SLOT,
            Capability {
                object: BLOCK_OBJECT_ID,
                rights: Rights::READ.union(Rights::WRITE),
            },
        )
        .map_err(|_| failure("setup", "capacidad bloques"))?;

    // Abrir volumen existente de T2.2 (formateado y poblado) y asegurar objetos.
    let mut volume = PermissionedVolume::open(&blk_caps, SESSION_BLOCK_SLOT, QA_DEVICE, DEV_PASSPHRASE)
        .map_err(|_| failure("setup", "abrir volumen cifrado"))?;

    // Escribir como root los objetos remotos requeridos
    volume
        .write(&root, &blk_caps, SESSION_BLOCK_SLOT, OBJ_PUBLIC_NOTICE, b"remote-notice-v1")
        .map_err(|_| failure("setup", "escribir OBJ_PUBLIC_NOTICE"))?;
    volume
        .write(&root, &blk_caps, SESSION_BLOCK_SLOT, OBJ_SHARED_CONFIG, b"remote-shared-v1")
        .map_err(|_| failure("setup", "escribir OBJ_SHARED_CONFIG"))?;

    // Armar estado TLS para procesar tráfico del peer host
    tls::prepare_for_selftest(root, alice, guest, volume, blk_caps);

    // Bucle de sondeo: el peer host ejecuta handshake+app; aquí respondemos.
    let mut progress = Progress::default();
    let mut spins = 0u32;
    while spins < POLL_ITERATIONS {
        // procesar RX/TX de la pila; el dispatch a TLS ocurre en net::handle_udp
        let _ = net::poll(&mut progress);
        if tls::all_passed() {
            break;
        }
        spins += 1;
        if spins & 0x3fff == 0 {
            core::hint::spin_loop();
        }
    }

    let (hs, fin, allowed, denied, replay) = tls::selftest_outcome();
    if !hs {
        return Err(failure("TLS 1.3 handshake", "no se completó el handshake"));
    }
    let _ = writeln!(console, "T2.3: TLS 1.3 handshake PASS");
    if !fin {
        return Err(failure("Finished verified", "Finished del cliente no verificado"));
    }
    let _ = writeln!(console, "T2.3: Finished verified PASS");
    if !allowed {
        return Err(failure("encrypted read allowed", "lectura autorizada no completada"));
    }
    let _ = writeln!(console, "T2.3: encrypted read allowed PASS");
    if !denied {
        return Err(failure("encrypted read denied", "lectura denegada no observada"));
    }
    let _ = writeln!(console, "T2.3: encrypted read denied PASS");
    if !replay {
        return Err(failure("replay rejected", "replay no detectado/rechazado"));
    }
    let _ = writeln!(console, "T2.3: replay rejected PASS");
    let _ = writeln!(console, "T2.3: remote session tests PASS");
    Ok(())
}
