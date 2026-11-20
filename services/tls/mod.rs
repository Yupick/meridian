//! Acceso remoto cifrado T2.3: perfil TLS 1.3 PSK sobre UDP/IPv4.
//!
//! Reutiliza la NIC/stack de T2.1 (mismo backend UDP dgram 127.0.0.1:55125<->55124)
//! y el puerto [`crate::net::TLS_PORT`] = 9443. El tráfico de aplicación transporta
//! solicitudes de login/lectura remota autenticadas y autorizadas contra las
//! sesiones de T2.2 (`auth`/`perm`) sobre `vol::PermissionedVolume`.
//!
//! # Perfil criptográfico (dev-only, QA)
//! - PSK = `SHA256(b"meridian-tls13-dev-psk-v1")` (32 bytes, fijo). **No apto para
//!   producción.**
//! - HKDF-SHA256/HMAC-SHA256 propios sobre `meridian_crypto::sha256`.
//! - Cifrado de registros: AES-128-GCM (`meridian_crypto::gcm::Aes128Gcm`).
//! - Nonce = IV XOR seq (seq big-endian en los últimos 8 bytes), seq separado por
//!   dirección empezando en 1. AAD = cabecera TLS de 5 bytes (legacy 0x0303).
//!
//! Nunca se imprimen claves, PSK, plaintexts de objetos ni tráfico descifrado.

use core::cell::UnsafeCell;

use meridian_crypto::gcm::Aes128Gcm;
use meridian_crypto::sha256::{self, Sha256};

use crate::auth::Session;
use crate::blk::SESSION_BLOCK_SLOT;
use crate::cap::{CapTable, Rights};
use crate::net::{self, Progress, TLS_PORT};
use crate::perm::PermissionedVolume;

pub mod selftest;

const MAGIC: &[u8; 8] = b"MRSTLS13";
const CT_HANDSHAKE: u8 = 0x16;
const CT_APP: u8 = 0x17;
// Spec: Finished record cifrado content_type=20
const CT_FINISHED: u8 = 20;
// Alerta de replay (record cifrado content_type=21, plaintext b"replay")
const CT_ALERT_APP: u8 = 21;

const CH_TYPE: u8 = 1;
const SH_TYPE: u8 = 2;

const CH_PAYLOAD_LEN: usize = 8 + 1 + 32 + 1; // 42
const SH_PAYLOAD_LEN: usize = 8 + 1 + 32 + 1 + 16; // 58
const CH_RECORD_LEN: usize = 5 + CH_PAYLOAD_LEN;
const SH_RECORD_LEN: usize = 5 + SH_PAYLOAD_LEN;

const MAX_RECORD: usize = 256;
const MAX_PLAINTEXT: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    WaitClientHello,
    WaitFinished,
    Established,
}

#[derive(Clone, Copy)]
struct Keys {
    client_key: [u8; 16],
    client_iv: [u8; 12],
    server_key: [u8; 16],
    server_iv: [u8; 12],
    finished_key: [u8; 32],
}

impl Keys {
    const fn zero() -> Self {
        Self {
            client_key: [0; 16],
            client_iv: [0; 12],
            server_key: [0; 16],
            server_iv: [0; 12],
            finished_key: [0; 32],
        }
    }
}

struct TlsState {
    phase: Phase,
    peer_ip: [u8; 4],
    peer_port: u16,
    transcript_hash: [u8; 32],
    keys: Keys,
    client_seq: u64, // next expected/used? records encrypted use seq starting at 1
    server_seq: u64,
    // seen request seq numbers (plaintext seq u64) to detect replay
    seen_req_seq: [u64; 8],
    seen_req_count: usize,
    finished_seen: bool,
    // outcomes for selftest
    handshake_ok: bool,
    finished_ok: bool,
    read_allowed_ok: bool,
    read_denied_ok: bool,
    replay_ok: bool,
    // sessions preloaded for remote authz (resolved by username)
    root_sess: Option<Session>,
    alice_sess: Option<Session>,
    guest_sess: Option<Session>,
    // volume and caps
    volume_ready: bool,
    volume: Option<PermissionedVolume>,
    caps: CapTable,
}

impl TlsState {
    const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            peer_ip: [0; 4],
            peer_port: 0,
            transcript_hash: [0; 32],
            keys: Keys::zero(),
            client_seq: 1,
            server_seq: 1,
            seen_req_seq: [0; 8],
            seen_req_count: 0,
            finished_seen: false,
            handshake_ok: false,
            finished_ok: false,
            read_allowed_ok: false,
            read_denied_ok: false,
            replay_ok: false,
            root_sess: None,
            alice_sess: None,
            guest_sess: None,
            volume_ready: false,
            volume: None,
            caps: CapTable::new(),
        }
    }

    fn reset_handshake(&mut self) {
        self.phase = Phase::WaitClientHello;
        self.peer_ip = [0; 4];
        self.peer_port = 0;
        self.transcript_hash = [0; 32];
        self.keys = Keys::zero();
        self.client_seq = 1;
        self.server_seq = 1;
        self.seen_req_seq = [0; 8];
        self.seen_req_count = 0;
        self.finished_seen = false;
        self.handshake_ok = false;
        self.finished_ok = false;
        self.read_allowed_ok = false;
        self.read_denied_ok = false;
        self.replay_ok = false;
    }
}

struct GlobalState(UnsafeCell<TlsState>);
// SAFETY: kernel mono-CPU, sin reentrancia/IRQ concurrente en este flujo.
unsafe impl Sync for GlobalState {}

static STATE: GlobalState = GlobalState(UnsafeCell::new(TlsState::new()));

fn state() -> &'static mut TlsState {
    // SAFETY: acceso exclusivo en contexto mono-CPU del kernel.
    unsafe { &mut *STATE.0.get() }
}

/// Inicializa/arma el estado TLS para la auto-prueba (llamado por selftest).
pub fn prepare_for_selftest(
    root: Session,
    alice: Session,
    guest: Session,
    volume: PermissionedVolume,
    caps: CapTable,
) {
    let st = state();
    st.root_sess = Some(root);
    st.alice_sess = Some(alice);
    st.guest_sess = Some(guest);
    st.volume = Some(volume);
    st.caps = caps;
    st.volume_ready = true;
    st.reset_handshake();
}

pub fn selftest_outcome() -> (bool, bool, bool, bool, bool) {
    let st = state();
    (
        st.handshake_ok,
        st.finished_ok,
        st.read_allowed_ok,
        st.read_denied_ok,
        st.replay_ok,
    )
}

pub fn all_passed() -> bool {
    let st = state();
    st.handshake_ok && st.finished_ok && st.read_allowed_ok && st.read_denied_ok && st.replay_ok
}

// ---- Crypto helpers (HMAC-SHA256 / HKDF-SHA256) ----

fn hmac_sha256(key: &[u8], data: &[u8], out: &mut [u8; 32]) {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let kh = sha256::hash(key);
        k[..32].copy_from_slice(&kh);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    let mut i = 0;
    while i < 64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
        i += 1;
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(data);
    let mut ih = [0u8; 32];
    inner.finalize(&mut ih);
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&ih);
    outer.finalize(out);
}

fn hkdf_extract(salt: &[u8], ikm: &[u8], out: &mut [u8; 32]) {
    // if salt is empty, use zeros hash len; but we always pass explicit salt
    if salt.is_empty() {
        let zeros = [0u8; 32];
        hmac_sha256(&zeros, ikm, out);
    } else {
        hmac_sha256(salt, ikm, out);
    }
}

fn hkdf_expand(prk: &[u8; 32], info: &[u8], out: &mut [u8]) {
    // out len <= 32 in our use; single block T(1)=HMAC(prk, info||0x01)
    let mut buf = [0u8; 160];
    let ilen = info.len();
    if ilen + 1 > buf.len() {
        return;
    }
    buf[..ilen].copy_from_slice(info);
    buf[ilen] = 0x01;
    let mut t = [0u8; 32];
    hmac_sha256(prk, &buf[..ilen + 1], &mut t);
    let n = core::cmp::min(out.len(), 32);
    out[..n].copy_from_slice(&t[..n]);
}

fn ct_eq16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut d = 0u8;
    let mut i = 0;
    while i < 16 {
        d |= a[i] ^ b[i];
        i += 1;
    }
    d == 0
}

fn psk_dev() -> [u8; 32] {
    sha256::hash(b"meridian-tls13-dev-psk-v1")
}

// ---- Record helpers ----

fn make_nonce(iv: &[u8; 12], seq: u64) -> [u8; 12] {
    let mut nonce = *iv;
    let s = seq.to_be_bytes();
    let mut i = 0;
    while i < 8 {
        nonce[4 + i] ^= s[i];
        i += 1;
    }
    nonce
}

fn seal_record(
    key: &[u8; 16],
    iv: &[u8; 12],
    seq: u64,
    content_type: u8,
    plaintext: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    let ct_len = plaintext.len() + 16;
    let total = 5 + ct_len;
    if total > out.len() || ct_len > 0xffff {
        return None;
    }
    out[0] = content_type;
    out[1] = 0x03;
    out[2] = 0x03;
    out[3] = (ct_len >> 8) as u8;
    out[4] = (ct_len & 0xff) as u8;
    let mut aad = [0u8; 5];
    aad.copy_from_slice(&out[..5]);
    let nonce = make_nonce(iv, seq);
    let gcm = Aes128Gcm::new(key);
    let mut tag = [0u8; 16];
    // split out to satisfy borrow checker
    let (hdr, rest) = out.split_at_mut(5);
    let _ = hdr;
    let (ct_out, _) = rest.split_at_mut(ct_len);
    let (c_only, tag_out) = ct_out.split_at_mut(plaintext.len());
    gcm.encrypt(&nonce, &aad, plaintext, c_only, &mut tag);
    tag_out.copy_from_slice(&tag);
    Some(total)
}

fn open_record(
    key: &[u8; 16],
    iv: &[u8; 12],
    seq: u64,
    record: &[u8],
    out_pt: &mut [u8],
) -> Option<(u8, usize)> {
    if record.len() < 5 + 16 {
        return None;
    }
    let ctype = record[0];
    if record[1] != 0x03 || record[2] != 0x03 {
        return None;
    }
    let len = u16::from_be_bytes([record[3], record[4]]) as usize;
    if 5 + len > record.len() || len < 16 || len - 16 > out_pt.len() {
        return None;
    }
    let mut aad = [0u8; 5];
    aad.copy_from_slice(&record[..5]);
    let ciphertext = &record[5..5 + len - 16];
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&record[5 + len - 16..5 + len]);
    let nonce = make_nonce(iv, seq);
    let gcm = Aes128Gcm::new(key);
    let ok = gcm.decrypt(&nonce, &aad, ciphertext, &tag, &mut out_pt[..ciphertext.len()]);
    if !ok {
        return None;
    }
    Some((ctype, ciphertext.len()))
}

fn send_udp(dst_ip: [u8; 4], dst_port: u16, data: &[u8]) {
    // ignore errors in best-effort response path
    let _ = net::send_udp(TLS_PORT, dst_ip, dst_port, data);
}

fn send_alert_replay(st: &TlsState) {
    let mut rec = [0u8; 64];
    if let Some(n) = seal_record(
        &st.keys.server_key,
        &st.keys.server_iv,
        st.server_seq,
        CT_ALERT_APP,
        b"replay",
        &mut rec,
    ) {
        // do not increment server_seq permanently in struct here? caller can; but easier keep simple
        // we need to send with current seq; caller will not reuse this seq for other things
        send_udp(st.peer_ip, st.peer_port, &rec[..n]);
    }
}

// ---- Protocol handling ----

fn handle_client_hello(st: &mut TlsState, src_ip: [u8; 4], src_port: u16, record: &[u8]) {
    if record.len() != CH_RECORD_LEN {
        return;
    }
    if record[0] != CT_HANDSHAKE || record[1] != 0x03 || record[2] != 0x03 {
        return;
    }
    let rlen = u16::from_be_bytes([record[3], record[4]]) as usize;
    if rlen != CH_PAYLOAD_LEN {
        return;
    }
    let payload = &record[5..5 + CH_PAYLOAD_LEN];
    if &payload[0..8] != MAGIC || payload[8] != CH_TYPE || payload[41] != 0 {
        return;
    }
    let mut client_random = [0u8; 32];
    let mut i = 0usize;
    while i < 32 {
        client_random[i] = payload[9 + i];
        i += 1;
    }

    // server_random = sha256(b"meridian-tls-server-random-v1" || client_random)
    let mut sr_hasher = Sha256::new();
    sr_hasher.update(b"meridian-tls-server-random-v1");
    sr_hasher.update(&client_random);
    let mut server_random = [0u8; 32];
    sr_hasher.finalize(&mut server_random);

    // transcript prefix = CH.payload || SH.payload[..41] (sin verify)
    let mut prefix = [0u8; CH_PAYLOAD_LEN + 41];
    i = 0;
    while i < CH_PAYLOAD_LEN {
        prefix[i] = payload[i];
        i += 1;
    }
    // SH payload primeros 41 bytes: magic(8) + type(1) + server_random(32)
    let base = CH_PAYLOAD_LEN;
    i = 0;
    while i < 8 {
        prefix[base + i] = MAGIC[i];
        i += 1;
    }
    prefix[base + 8] = SH_TYPE;
    i = 0;
    while i < 32 {
        prefix[base + 9 + i] = server_random[i];
        i += 1;
    }
    let transcript_hash = sha256::hash(&prefix);

    // key schedule
    let psk = psk_dev();
    let zeros = [0u8; 32];
    let mut early = [0u8; 32];
    hkdf_extract(&zeros, &psk, &mut early);
    let mut hs_info = [0u8; 24 + 32];
    let label = b"meridian tls13 handshake";
    i = 0;
    while i < 24 {
        hs_info[i] = label[i];
        i += 1;
    }
    i = 0;
    while i < 32 {
        hs_info[24 + i] = transcript_hash[i];
        i += 1;
    }
    let mut hs = [0u8; 32];
    hkdf_expand(&early, &hs_info, &mut hs);

    let mut keys = Keys::zero();
    hkdf_expand(&hs, b"meridian tls13 c key", &mut keys.client_key);
    hkdf_expand(&hs, b"meridian tls13 c iv", &mut keys.client_iv);
    hkdf_expand(&hs, b"meridian tls13 s key", &mut keys.server_key);
    hkdf_expand(&hs, b"meridian tls13 s iv", &mut keys.server_iv);
    hkdf_expand(&hs, b"meridian tls13 finished", &mut keys.finished_key);

    // verify = HMAC(finished_key, transcript_hash)[..16]
    let mut verify_full = [0u8; 32];
    hmac_sha256(&keys.finished_key, &transcript_hash, &mut verify_full);

    // build ServerHello record
    let mut sh = [0u8; SH_RECORD_LEN];
    sh[0] = CT_HANDSHAKE;
    sh[1] = 0x03;
    sh[2] = 0x03;
    sh[3] = (SH_PAYLOAD_LEN >> 8) as u8;
    sh[4] = (SH_PAYLOAD_LEN & 0xff) as u8;
    let p = &mut sh[5..];
    p[0..8].copy_from_slice(MAGIC);
    p[8] = SH_TYPE;
    p[9..41].copy_from_slice(&server_random);
    p[41] = 0;
    p[42..58].copy_from_slice(&verify_full[..16]);

    // commit state
    st.peer_ip = src_ip;
    st.peer_port = src_port;
    st.transcript_hash = transcript_hash;
    st.keys = keys;
    st.client_seq = 1;
    st.server_seq = 1;
    st.seen_req_seq = [0; 8];
    st.seen_req_count = 0;
    st.finished_seen = false;
    st.phase = Phase::WaitFinished;
    st.handshake_ok = true;

    send_udp(src_ip, src_port, &sh);
}

fn session_for_user<'a>(st: &'a TlsState, user: &[u8]) -> Option<&'a Session> {
    if let Some(s) = st.alice_sess.as_ref() {
        if s.username_bytes() == user { return Some(s); }
    }
    if let Some(s) = st.root_sess.as_ref() {
        if s.username_bytes() == user { return Some(s); }
    }
    if let Some(s) = st.guest_sess.as_ref() {
        if s.username_bytes() == user { return Some(s); }
    }
    None
}

fn mark_req_seq_seen(st: &mut TlsState, seq: u64) -> bool {
    let mut i = 0;
    while i < st.seen_req_count {
        if st.seen_req_seq[i] == seq {
            return false;
        }
        i += 1;
    }
    if st.seen_req_count < st.seen_req_seq.len() {
        st.seen_req_seq[st.seen_req_count] = seq;
        st.seen_req_count += 1;
        true
    } else {
        // table full: treat as replay/deny to be safe
        false
    }
}

fn handle_finished(st: &mut TlsState, record: &[u8]) {
    if st.phase != Phase::WaitFinished {
        return;
    }
    if st.finished_seen {
        // replay of finished
        st.replay_ok = true;
        send_alert_replay(st);
        st.server_seq = st.server_seq.wrapping_add(1);
        return;
    }
    let mut pt = [0u8; 64];
    let res = open_record(&st.keys.client_key, &st.keys.client_iv, st.client_seq, record, &mut pt);
    let (ctype, plen) = match res {
        Some(v) => v,
        None => return, // ignore invalid
    };
    st.client_seq = st.client_seq.wrapping_add(1);
    if ctype != CT_FINISHED || plen != 16 {
        return;
    }
    let mut expect_full = [0u8; 32];
    let mut data = [0u8; 15 + 32]; // "client finished" (15) + transcript_hash
    data[..15].copy_from_slice(b"client finished");
    data[15..47].copy_from_slice(&st.transcript_hash);
    hmac_sha256(&st.keys.finished_key, &data[..47], &mut expect_full);
    let mut expect = [0u8; 16];
    expect.copy_from_slice(&expect_full[..16]);
    let mut got = [0u8; 16];
    got.copy_from_slice(&pt[..16]);
    if ct_eq16(&expect, &got) {
        st.finished_seen = true;
        st.finished_ok = true;
        st.phase = Phase::Established;
    }
}

fn handle_app(st: &mut TlsState, record: &[u8]) {
    if st.phase != Phase::Established {
        // if still in WaitFinished a client might retransmit? ignore
        return;
    }
    let mut pt = [0u8; MAX_PLAINTEXT];
    let res = open_record(&st.keys.client_key, &st.keys.client_iv, st.client_seq, record, &mut pt);
    let (ctype, plen) = match res {
        Some(v) => v,
        None => return,
    };
    st.client_seq = st.client_seq.wrapping_add(1);
    if ctype != CT_APP || plen < 4 {
        // a Finished replay sent as encrypted record after established?
        if ctype == CT_FINISHED {
            st.replay_ok = true;
            send_alert_replay(st);
            st.server_seq = st.server_seq.wrapping_add(1);
        }
        return;
    }
    if &pt[0..4] != b"REQ\0" {
        return;
    }
    // parse: REQ\0 | user_len u8 | user | key u64 BE | rights u8 | seq u64 BE
    if plen < 5 {
        return;
    }
    let user_len = pt[4] as usize;
    let need = 5 + user_len + 8 + 1 + 8;
    if plen < need || user_len == 0 || user_len > 32 {
        return;
    }
    let user = &pt[5..5 + user_len];
    let mut off = 5 + user_len;
    let mut key_bytes = [0u8; 8];
    key_bytes.copy_from_slice(&pt[off..off + 8]);
    let obj_key = u64::from_be_bytes(key_bytes);
    off += 8;
    let rights_byte = pt[off];
    off += 1;
    let mut seq_bytes = [0u8; 8];
    seq_bytes.copy_from_slice(&pt[off..off + 8]);
    let req_seq = u64::from_be_bytes(seq_bytes);

    if !mark_req_seq_seen(st, req_seq) {
        st.replay_ok = true;
        send_alert_replay(st);
        st.server_seq = st.server_seq.wrapping_add(1);
        return;
    }

    let req_rights = match rights_byte {
        1 => Rights::READ,
        2 => Rights::WRITE,
        _ => Rights::READ,
    };

    let mut status: u8 = 0;
    let mut value = [0u8; 128];
    let mut value_len: usize = 0;

    let sess_opt = session_for_user(st, user).copied();
    if let Some(sess) = sess_opt {
        // only READ supported for remote read test; enforce perm::check
        if st.volume_ready {
            if let Some(vol) = st.volume.as_mut() {
                if req_rights.contains(Rights::READ) {
                    match vol.read(&sess, &st.caps, SESSION_BLOCK_SLOT, obj_key, &mut value) {
                        Ok(n) => {
                            status = 1;
                            value_len = n;
                        }
                        Err(_e) => {
                            status = 0;
                            value_len = 0;
                        }
                    }
                } else {
                    // treat non-read as denied in this profile
                    status = 0;
                }
            }
        }
    } else {
        status = 0;
    }

    // build response plaintext
    // RES\0 | status u8 | user_len u8 | user | key u64 BE | value_len u16 BE | value
    let mut resp = [0u8; MAX_PLAINTEXT];
    let mut r = 0usize;
    resp[r..r + 4].copy_from_slice(b"RES\0");
    r += 4;
    resp[r] = status;
    r += 1;
    resp[r] = user_len as u8;
    r += 1;
    resp[r..r + user_len].copy_from_slice(user);
    r += user_len;
    resp[r..r + 8].copy_from_slice(&obj_key.to_be_bytes());
    r += 8;
    let vlen16 = value_len as u16;
    resp[r..r + 2].copy_from_slice(&vlen16.to_be_bytes());
    r += 2;
    if value_len > 0 && r + value_len <= resp.len() {
        resp[r..r + value_len].copy_from_slice(&value[..value_len]);
        r += value_len;
    }

    let mut rec = [0u8; MAX_RECORD];
    if let Some(n) = seal_record(
        &st.keys.server_key,
        &st.keys.server_iv,
        st.server_seq,
        CT_APP,
        &resp[..r],
        &mut rec,
    ) {
        st.server_seq = st.server_seq.wrapping_add(1);
        send_udp(st.peer_ip, st.peer_port, &rec[..n]);

        // mark outcomes based on request/response for known QA cases
        if user == b"alice" && obj_key == 13 && req_rights.contains(Rights::READ) && status == 1 {
            if value_len == b"remote-notice-v1".len() && &value[..value_len] == b"remote-notice-v1" {
                st.read_allowed_ok = true;
            }
        }
        if user == b"guest" && obj_key == 10 && req_rights.contains(Rights::READ) && status == 0 {
            st.read_denied_ok = true;
        }
    }
}

/// Punto de entrada llamado por `net::handle_udp` para datagramas UDP a 9443.
pub fn handle_datagram(src_ip: [u8; 4], src_port: u16, payload: &[u8], progress: &mut Progress) -> Result<(), net::NetStackError> {
    progress.tls_any = true;
    let st = state();
    if !st.volume_ready {
        return Ok(());
    }
    if payload.len() < 5 {
        return Ok(());
    }
    let ctype = payload[0];
    match st.phase {
        Phase::Idle => {
            // allow starting
            st.phase = Phase::WaitClientHello;
            handle_client_hello(st, src_ip, src_port, payload);
        }
        Phase::WaitClientHello => {
            if ctype == CT_HANDSHAKE {
                handle_client_hello(st, src_ip, src_port, payload);
            }
        }
        Phase::WaitFinished => {
            // expect encrypted Finished record (content_type 20 as per spec)
            if src_ip == st.peer_ip && src_port == st.peer_port {
                if ctype == CT_FINISHED {
                    handle_finished(st, payload);
                } else if ctype == CT_HANDSHAKE {
                    // retransmitted ClientHello? restart
                    handle_client_hello(st, src_ip, src_port, payload);
                }
            }
        }
        Phase::Established => {
            if src_ip == st.peer_ip && src_port == st.peer_port {
                if ctype == CT_APP || ctype == CT_FINISHED {
                    handle_app(st, payload);
                }
            }
        }
    }
    Ok(())
}
