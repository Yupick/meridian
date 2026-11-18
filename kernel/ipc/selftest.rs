//! Auto-pruebas de IPC por puertos y capacidades (A6).
//!
//! Se ejecutan en cada arranque, después de A4/A5, y comprueban los tres
//! criterios de aceptación:
//!
//! 1. un proceso envía un mensaje a otro por un puerto y el receptor lo recibe
//!    con contenido íntegro;
//! 2. un proceso sin el derecho necesario (o sin capacidad) obtiene un error
//!    controlado, no un éxito silencioso ni un pánico;
//! 3. derivar/reducir derechos funciona y cualquier intento de ampliación se
//!    rechaza.
//!
//! Los procesos son lógicos, con PML4 propia (A5) y tabla de capacidades (A6).
//! Las operaciones se ejecutan conmutando `CR3` al espacio del proceso que
//! actúa, igual que en la prueba de aislamiento de A5.

use core::fmt::Write;

use crate::cap::{CapError, Rights};
use crate::console::Console;
use crate::ipc::{self, IpcError, Message};
use crate::mm::paging;
use crate::proc::Process;

const TAG_PRIMARY: u64 = 0xA6_4950_4300_0001;
const TAG_DERIVED: u64 = 0xA6_4950_4300_0002;
const PAYLOAD_PRIMARY: &[u8] = b"meridian-a6-send";
const PAYLOAD_DERIVED: &[u8] = b"meridian-a6-derived";

/// Ejecuta la prueba A6 completa.
pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => {
            let _ = writeln!(
                console,
                "A6: ipc/capabilities tests PASS (send/recv through port, denial without rights, monotonic attenuation)"
            );
            true
        }
        Err(reason) => {
            let _ = writeln!(console, "A6: ipc/capabilities tests FAIL: {reason}");
            false
        }
    }
}

fn run_inner(console: &mut Console) -> Result<(), &'static str> {
    let kernel_root = paging::kernel_root();
    if kernel_root == 0 {
        return Err("kernel paging root no inicializado");
    }

    // Dos procesos lógicos con espacio de direcciones propio, como en A5.
    let root_a = paging::create_address_space().map_err(|_| "no se pudo crear espacio de A")?;
    let root_b = paging::create_address_space().map_err(|_| "no se pudo crear espacio de B")?;
    let mut proc_a = Process::new(1, root_a);
    let mut proc_b = Process::new(2, root_b);

    // Puerto propiedad de B. A recibe todos los derechos; B solo READ (puede
    // recibir, pero no enviar ni administrar el puerto).
    let port = ipc::create_port(proc_b.id).map_err(|_| "no se pudo crear el puerto")?;
    proc_a
        .grant(0, port, Rights::ALL)
        .map_err(|_| "no se pudo conceder la capacidad completa a A")?;
    proc_b
        .grant(0, port, Rights::READ)
        .map_err(|_| "no se pudo conceder la capacidad de lectura a B")?;

    let _ = writeln!(
        console,
        "A6: ipc port created (id={port}, owner=B, A[0] rights={}, B[0] rights={})",
        Rights::ALL,
        Rights::READ
    );

    // --- Criterio 1: A envía a B por el puerto y B recibe -----------------
    let primary =
        Message::with_bytes(TAG_PRIMARY, PAYLOAD_PRIMARY).map_err(|_| "mensaje primario grande")?;

    paging::switch(proc_a.root);
    let send_result = ipc::send(&proc_a.caps, 0, primary);
    paging::switch(kernel_root);
    let sent_to = send_result.map_err(|_| "el envío desde A falló")?;
    if sent_to != port {
        return Err("el mensaje se encoló en un puerto distinto");
    }

    paging::switch(proc_b.root);
    let recv_result = ipc::recv(&proc_b.caps, 0);
    paging::switch(kernel_root);
    let received = recv_result.map_err(|_| "la recepción en B falló")?;

    if received.tag != TAG_PRIMARY || received.bytes() != PAYLOAD_PRIMARY {
        let _ = writeln!(
            console,
            "A6: ipc send/recv test FAIL: tag=0x{:016X} len={} payload={:?}",
            received.tag,
            received.len(),
            received.bytes()
        );
        return Err("el mensaje recibido no coincide con el enviado");
    }

    let _ = writeln!(
        console,
        "A6: ipc send/recv test PASS (A root=0x{root_a:016X} -> port {port} -> B root=0x{root_b:016X}, tag=0x{TAG_PRIMARY:016X}, {} bytes)",
        received.len()
    );

    // --- Criterio 2: B no tiene SEND/WRITE/EXECUTE -----------------------
    paging::switch(proc_b.root);
    let denied_send = ipc::send(&proc_b.caps, 0, primary);
    let denied_clear = ipc::clear_queue(&proc_b.caps, 0);
    let denied_invoke = ipc::invoke(&proc_b.caps, 0);
    let missing = ipc::send(&proc_b.caps, 9, primary);
    paging::switch(kernel_root);

    if !is_rights_denied(denied_send)
        || !is_rights_denied(denied_clear)
        || !is_rights_denied(denied_invoke)
    {
        return Err("un proceso sin derechos no recibió RightsDenied");
    }
    if missing.err() != Some(IpcError::Capability(CapError::EmptySlot)) {
        return Err("un slot vacío no devolvió EmptySlot");
    }

    let _ = writeln!(
        console,
        "A6: capability denied without rights PASS (B[0] rights={}: send/clear/invoke -> RightsDenied, slot 9 -> EmptySlot)",
        Rights::READ
    );

    // --- Criterio 3: reducción monotónica y rechazo de ampliación --------
    let reduced_rights = Rights::READ.union(Rights::SEND);
    let derived = proc_a
        .caps
        .derive(0, 1, reduced_rights)
        .map_err(|_| "la derivación reductora falló")?;
    if derived.object != port || derived.rights != reduced_rights {
        return Err("la capacidad derivada no conserva objeto/derechos esperados");
    }

    // La capacidad derivada sigue sirviendo para enviar.
    let derived_message =
        Message::with_bytes(TAG_DERIVED, PAYLOAD_DERIVED).map_err(|_| "mensaje derivado grande")?;
    paging::switch(proc_a.root);
    let derived_send = ipc::send(&proc_a.caps, 1, derived_message);
    paging::switch(kernel_root);
    derived_send.map_err(|_| "el envío con la capacidad derivada falló")?;

    paging::switch(proc_b.root);
    let derived_recv = ipc::recv(&proc_b.caps, 0);
    paging::switch(kernel_root);
    let received_derived = derived_recv.map_err(|_| "la recepción derivada falló")?;
    if received_derived.tag != TAG_DERIVED || received_derived.bytes() != PAYLOAD_DERIVED {
        return Err("el mensaje derivado no coincide con el enviado");
    }

    // Reducción in-place: slot 1 pasa de READ|SEND a READ.
    let attenuated = proc_a
        .caps
        .attenuate(1, Rights::READ)
        .map_err(|_| "la reducción in-place falló")?;
    if attenuated.rights != Rights::READ {
        return Err("la capacidad atenuada no quedó con READ");
    }

    // Ya reducida, enviar por el slot 1 debe fallar.
    paging::switch(proc_a.root);
    let reduced_send = ipc::send(&proc_a.caps, 1, derived_message);
    let reduced_invoke = ipc::invoke(&proc_a.caps, 1);
    paging::switch(kernel_root);
    if !is_rights_denied(reduced_send) {
        return Err("se permitió enviar con una capacidad atenuada sin SEND");
    }
    if !is_rights_denied(reduced_invoke) {
        return Err("se permitió invocar con una capacidad atenuada sin EXECUTE");
    }

    // Intento de derivar con un derecho no presente (EXECUTE) -> rechazo.
    let escalation = proc_a.caps.derive(
        1,
        2,
        Rights::READ.union(Rights::SEND).union(Rights::EXECUTE),
    );
    if escalation != Err(CapError::RightsEscalation) {
        return Err("se aceptó una derivación que ampliaba derechos");
    }
    if proc_a.caps.get(2) != Err(CapError::EmptySlot) {
        return Err("el slot rechazado quedó ocupado tras la ampliación fallida");
    }

    // Intento de ampliar in-place -> rechazo y slot intacto.
    let escalate_in_place = proc_a.caps.attenuate(1, Rights::ALL);
    if escalate_in_place != Err(CapError::RightsEscalation) {
        return Err("se aceptó una atenuación que ampliaba derechos");
    }
    match proc_a.caps.get(1) {
        Ok(capability) if capability.rights == Rights::READ => {}
        _ => return Err("la capacidad original cambió tras el intento de ampliación"),
    }

    // La capacidad original (slot 0) sigue intacta con todos los derechos.
    match proc_a.caps.get(0) {
        Ok(capability) if capability.rights == Rights::ALL && capability.object == port => {}
        _ => return Err("la capacidad origen se modificó al derivar/atenuar"),
    }

    let _ = writeln!(
        console,
        "A6: capability reduction/attenuation PASS (slot0 {} -> slot1 {} derivado y luego atenuado a {}; EXECUTE ampliado rechazado, ampliacion in-place rechazada, origen intacto)",
        Rights::ALL,
        reduced_rights,
        Rights::READ
    );

    paging::switch(kernel_root);
    Ok(())
}

/// `true` si la operación devolvió exactamente `RightsDenied`.
fn is_rights_denied<T>(result: Result<T, IpcError>) -> bool {
    result.err() == Some(IpcError::Capability(CapError::RightsDenied))
}
