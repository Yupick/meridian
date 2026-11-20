//! Auto-pruebas de usuarios con permisos sobre el volumen cifrado (T2.2).
//!
//! Emite los seis marcadores exigidos por QA por COM1 (a través de
//! `Console`). Cualquier fallo imprime `T2.2: <step> FAIL: <msg>`. Nunca se
//! imprimen la passphrase, la clave derivada ni el ciphertext: solo se
//! comparan longitudes y contenidos de plaintext conocidos.

use core::fmt::Write;

use crate::auth::{self, GROUP_ADMIN, GROUP_AUDIT, GROUP_GUEST, GROUP_USERS};
use crate::blk::{self, BlockDeviceId, BLOCK_OBJECT_ID, SESSION_BLOCK_SLOT};
use crate::cap::{Capability, CapTable};
use crate::console::Console;
use crate::fs;
use crate::vol::DEV_PASSPHRASE;

use super::{
    check, PermError, PermissionedVolume, Rights, OBJ_AUDIT_LOG, OBJ_PUBLIC_NOTICE,
    OBJ_ROOT_SECRET, OBJ_SHARED_CONFIG,
};

const QA_DEVICE: BlockDeviceId = 1;

const SHARED_V1: &[u8] = b"shared-config-v1";
const SHARED_V2: &[u8] = b"shared-config-v2";
const AUDIT_V1: &[u8] = b"audit-log-v1";
const SECRET_V1: &[u8] = b"root-secret-v1";
const NOTICE_V1: &[u8] = b"public-notice-v1";

struct Failure {
    step: &'static str,
    message: &'static str,
}

fn failure(step: &'static str, message: &'static str) -> Failure {
    Failure { step, message }
}

fn error_message(error: PermError) -> &'static str {
    match error {
        PermError::UnknownObject => "objeto sin ACL",
        PermError::AccessDenied => "acceso denegado por ACL",
        PermError::Volume(_) => "error del volumen cifrado",
    }
}

fn expect_denied<T>(step: &'static str, result: Result<T, PermError>) -> Result<(), Failure> {
    match result {
        Err(PermError::AccessDenied) => Ok(()),
        Err(error) => Err(failure(step, error_message(error))),
        Ok(_) => Err(failure(step, "la ACL permitió un acceso denegado")),
    }
}

fn expect_read(
    step: &'static str,
    volume: &PermissionedVolume,
    session: &auth::Session,
    caps: &CapTable,
    key: u64,
    expected: &[u8],
) -> Result<(), Failure> {
    let mut out = [0u8; 64];
    let len = volume
        .read(session, caps, SESSION_BLOCK_SLOT, key, &mut out)
        .map_err(|error| failure(step, error_message(error)))?;
    if len != expected.len() || &out[..len] != expected {
        return Err(failure(
            step,
            "la lectura no devolvió el plaintext esperado",
        ));
    }
    Ok(())
}

/// Ejecuta la prueba T2.2 completa.
pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(failed) => {
            let _ = writeln!(console, "T2.2: {} FAIL: {}", failed.step, failed.message);
            false
        }
    }
}

fn run_inner(console: &mut Console) -> Result<(), Failure> {
    // FS y usuarios limpios: A8/A9 dejaron su propio estado y aquí necesitamos
    // las cuatro identidades de QA con sus grupos inferidos del nombre.
    fs::init();
    auth::reset_users();
    let mut fs_caps = CapTable::new();
    fs_caps
        .insert(
            auth::SESSION_FS_SLOT,
            Capability {
                object: fs::FS_OBJECT_ID,
                rights: Rights::READ.union(Rights::WRITE),
            },
        )
        .map_err(|_| failure("setup", "no se pudo crear capacidad RW para FS"))?;
    auth::register_user(&fs_caps, auth::SESSION_FS_SLOT, "root", "meridian", 0xB201)
        .map_err(|_| failure("setup", "no se pudo registrar root"))?;
    auth::register_user(&fs_caps, auth::SESSION_FS_SLOT, "alice", "secret", 0xB202)
        .map_err(|_| failure("setup", "no se pudo registrar alice"))?;
    auth::register_user(&fs_caps, auth::SESSION_FS_SLOT, "bob", "builder", 0xB203)
        .map_err(|_| failure("setup", "no se pudo registrar bob"))?;
    auth::register_user(&fs_caps, auth::SESSION_FS_SLOT, "guest", "visitor", 0xB204)
        .map_err(|_| failure("setup", "no se pudo registrar guest"))?;

    let root = auth::login("root", "meridian", &fs_caps, auth::SESSION_FS_SLOT)
        .map_err(|_| failure("setup", "login de root rechazado"))?;
    let alice = auth::login("alice", "secret", &fs_caps, auth::SESSION_FS_SLOT)
        .map_err(|_| failure("setup", "login de alice rechazado"))?;
    let bob = auth::login("bob", "builder", &fs_caps, auth::SESSION_FS_SLOT)
        .map_err(|_| failure("setup", "login de bob rechazado"))?;
    let guest = auth::login("guest", "visitor", &fs_caps, auth::SESSION_FS_SLOT)
        .map_err(|_| failure("setup", "login de guest rechazado"))?;

    // 1. Los grupos inferidos y copiados a la sesión son los esperados.
    if root.groups != GROUP_ADMIN | GROUP_USERS
        || alice.groups != GROUP_USERS
        || bob.groups != GROUP_USERS | GROUP_AUDIT
        || guest.groups != GROUP_GUEST
    {
        return Err(failure(
            "group assignment",
            "los grupos de las sesiones son incorrectos",
        ));
    }
    let _ = writeln!(console, "T2.2: group assignment PASS");

    // Capacidad RW sobre el servicio de bloques, igual que en vol/selftest:
    // la cap FS/block no se omite en ninguna operación del volumen.
    let mut caps = CapTable::new();
    caps.insert(
        SESSION_BLOCK_SLOT,
        Capability {
            object: BLOCK_OBJECT_ID,
            rights: Rights::READ.union(Rights::WRITE),
        },
    )
    .map_err(|_| failure("capabilities", "no se pudo crear capacidad RW de bloques"))?;
    if blk::sector_count(QA_DEVICE) < 64 {
        return Err(failure(
            "format",
            "el disco QA tiene menos de 64 sectores",
        ));
    }

    let mut volume = PermissionedVolume::format(
        &caps,
        SESSION_BLOCK_SLOT,
        QA_DEVICE,
        DEV_PASSPHRASE,
    )
    .map_err(|error| failure("format", error_message(error)))?;

    // Poblado inicial: solo ADMIN escribe. Los valores son plaintext de QA.
    volume
        .write(
            &root,
            &caps,
            SESSION_BLOCK_SLOT,
            OBJ_SHARED_CONFIG,
            SHARED_V1,
        )
        .map_err(|error| failure("format", error_message(error)))?;
    volume
        .write(&root, &caps, SESSION_BLOCK_SLOT, OBJ_AUDIT_LOG, AUDIT_V1)
        .map_err(|error| failure("format", error_message(error)))?;
    volume
        .write(&root, &caps, SESSION_BLOCK_SLOT, OBJ_ROOT_SECRET, SECRET_V1)
        .map_err(|error| failure("format", error_message(error)))?;
    volume
        .write(
            &root,
            &caps,
            SESSION_BLOCK_SLOT,
            OBJ_PUBLIC_NOTICE,
            NOTICE_V1,
        )
        .map_err(|error| failure("format", error_message(error)))?;

    // 2. USERS puede leer la configuración compartida.
    expect_read(
        "ACL read allowed",
        &volume,
        &alice,
        &caps,
        OBJ_SHARED_CONFIG,
        SHARED_V1,
    )?;
    let _ = writeln!(console, "T2.2: ACL read allowed PASS");

    // 3. USERS no puede escribirla; ADMIN sí y el nuevo valor se persiste.
    expect_denied(
        "ACL write denied",
        volume.write(
            &alice,
            &caps,
            SESSION_BLOCK_SLOT,
            OBJ_SHARED_CONFIG,
            b"intrusion",
        ),
    )?;
    volume
        .write(
            &root,
            &caps,
            SESSION_BLOCK_SLOT,
            OBJ_SHARED_CONFIG,
            SHARED_V2,
        )
        .map_err(|error| failure("ACL write denied", error_message(error)))?;
    expect_read(
        "ACL write denied",
        &volume,
        &root,
        &caps,
        OBJ_SHARED_CONFIG,
        SHARED_V2,
    )?;
    let _ = writeln!(console, "T2.2: ACL write denied PASS");

    // 4. Aislamiento por grupo: AUDIT lee el log, USERS/GUEST no leen objetos
    // reservados y GUEST sí lee el aviso público.
    expect_read(
        "group isolation",
        &volume,
        &bob,
        &caps,
        OBJ_AUDIT_LOG,
        AUDIT_V1,
    )?;
    expect_denied(
        "group isolation",
        volume.read(&alice, &caps, SESSION_BLOCK_SLOT, OBJ_AUDIT_LOG, &mut [0u8; 64]),
    )?;
    expect_denied(
        "group isolation",
        volume.read(
            &guest,
            &caps,
            SESSION_BLOCK_SLOT,
            OBJ_SHARED_CONFIG,
            &mut [0u8; 64],
        ),
    )?;
    if check(&guest, OBJ_SHARED_CONFIG, Rights::READ) != Err(PermError::AccessDenied) {
        return Err(failure(
            "group isolation",
            "check() no denegó la lectura de GUEST",
        ));
    }
    expect_read(
        "group isolation",
        &volume,
        &guest,
        &caps,
        OBJ_PUBLIC_NOTICE,
        NOTICE_V1,
    )?;
    let _ = writeln!(console, "T2.2: group isolation PASS");

    // 5. Las ACLs se aplican también sobre el volumen reabierto: el estado
    // cifrado persistió y los grupos siguen limitando cada lectura.
    drop(volume);
    let reopened = PermissionedVolume::open(
        &caps,
        SESSION_BLOCK_SLOT,
        QA_DEVICE,
        DEV_PASSPHRASE,
    )
    .map_err(|error| failure("encrypted ACL persistence", error_message(error)))?;
    expect_read(
        "encrypted ACL persistence",
        &reopened,
        &root,
        &caps,
        OBJ_ROOT_SECRET,
        SECRET_V1,
    )?;
    expect_read(
        "encrypted ACL persistence",
        &reopened,
        &guest,
        &caps,
        OBJ_PUBLIC_NOTICE,
        NOTICE_V1,
    )?;
    let _ = writeln!(console, "T2.2: encrypted ACL persistence PASS");

    let _ = writeln!(console, "T2.2: permissions tests PASS");
    Ok(())
}
