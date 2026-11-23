//! Terminal/shell de texto alpha (A9).
//!
//! Interpreta comandos sobre una sesión autenticada (capacidades + cwd).
//! Sin sesión válida no se permite acceso a la terminal.

#![allow(dead_code)]

use core::fmt::Write;

use crate::auth::Session;
use crate::console::Console;
use crate::fs::{self, DirEntry, FsError, NodeId, NodeType, MAX_LIST_ENTRIES, MAX_NAME_LEN};

pub mod selftest;

const MAX_PATH: usize = 128;
const MAX_LINE: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExecResult {
    Continue,
    Exit,
}

/// Estado de la shell asociado a una sesión.
pub struct Shell {
    pub session: Session,
    path: [u8; MAX_PATH],
    path_len: usize,
}

impl Shell {
    /// Crea la shell a partir de una sesión autenticada; `cwd` inicial es el home.
    pub fn new(session: Session) -> Self {
        let mut path = [0u8; MAX_PATH];
        let hb = session.home_bytes();
        let n = if hb.len() > MAX_PATH { MAX_PATH } else { hb.len() };
        let mut i = 0; while i < n { path[i] = hb[i]; i += 1; }
        Self { session, path, path_len: n }
    }

    fn path_str(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or("/")
    }

    pub fn prompt(&self, console: &mut Console) {
        console.set_foreground(crate::console::ACCENT);
        let _ = write!(console, "meridian:{}$ ", self.path_str());
        console.set_foreground(crate::console::TEXT);
    }

    /// Ejecuta una línea de comando.
    pub fn execute(&mut self, console: &mut Console, line: &str) -> ExecResult {
        let line = trim(line);
        if line.is_empty() {
            return ExecResult::Continue;
        }
        // echo con redirección: echo texto > archivo
        if starts_with(line, "echo ") || line == "echo" {
            self.cmd_echo(console, line);
            return ExecResult::Continue;
        }
        // tokenizar simple por espacios
        let mut parts: [&str; 8] = [""; 8];
        let mut count = 0usize;
        let mut rest = line;
        while count < 8 {
            rest = trim_start(rest);
            if rest.is_empty() { break; }
            let (tok, r) = next_token(rest);
            parts[count] = tok;
            count += 1;
            rest = r;
        }
        let cmd = parts[0];
        match cmd {
            "help" => self.cmd_help(console),
            "clear" => { console.clear_screen(); }
            "exit" => {
                let _ = writeln!(console, "logout");
                return ExecResult::Exit;
            }
            "pwd" => { let _ = writeln!(console, "{}", self.path_str()); }
            "ls" => {
                let path = if count >= 2 { parts[1] } else { "." };
                self.cmd_ls(console, path);
            }
            "cd" => {
                let path = if count >= 2 { parts[1] } else { "/" };
                self.cmd_cd(console, path);
            }
            "mkdir" | "md" => {
                if count < 2 { let _ = writeln!(console, "mkdir: falta operando"); }
                else { self.cmd_mkdir(console, parts[1]); }
            }
            "rmdir" | "rd" => {
                if count < 2 { let _ = writeln!(console, "rmdir: falta operando"); }
                else { self.cmd_rmdir(console, parts[1]); }
            }
            "rm" => {
                if count < 2 { let _ = writeln!(console, "rm: falta operando"); }
                else if parts[1] == "-r" {
                    if count < 3 { let _ = writeln!(console, "rm: falta operando"); }
                    else { self.cmd_rm_r(console, parts[2]); }
                } else {
                    self.cmd_rm(console, parts[1]);
                }
            }
            "cat" => {
                if count < 2 { let _ = writeln!(console, "cat: falta operando"); }
                else { self.cmd_cat(console, parts[1]); }
            }
            _ => { let _ = writeln!(console, "{}: comando no encontrado", cmd); }
        }
        ExecResult::Continue
    }

    fn slot(&self) -> usize { crate::auth::SESSION_FS_SLOT }

    fn cmd_help(&self, console: &mut Console) {
        let _ = writeln!(console, "Comandos: ls, cd, pwd, mkdir/md, rmdir/rd, rm [-r], cat, echo texto > archivo, help, clear, exit");
    }

    fn cmd_ls(&self, console: &mut Console, path: &str) {
        let mut entries = [DirEntry { attr: fs::NodeAttr { name: [0; MAX_NAME_LEN], name_len: 0, node_type: NodeType::File, size: 0 } }; MAX_LIST_ENTRIES];
        match fs::ls(&self.session.caps, self.slot(), self.session.cwd, path, &mut entries) {
            Ok(count) => {
                let mut i = 0;
                while i < count {
                    let e = &entries[i];
                    let name = core::str::from_utf8(e.attr.name_bytes()).unwrap_or("?");
                    match e.attr.node_type {
                        NodeType::Directory => { let _ = writeln!(console, "{}/", name); }
                        NodeType::File => { let _ = writeln!(console, "{}\t({} bytes)", name, e.attr.size); }
                    }
                    i += 1;
                }
            }
            Err(e) => { let _ = writeln!(console, "ls: {}", fs_err_msg(e)); }
        }
    }

    fn cmd_cd(&mut self, console: &mut Console, path: &str) {
        match fs::cd(&self.session.caps, self.slot(), self.session.cwd, path) {
            Ok(new_cwd) => {
                self.session.cwd = new_cwd;
                self.update_path(path);
                let _ = writeln!(console, "{}", self.path_str());
            }
            Err(e) => { let _ = writeln!(console, "cd: {}", fs_err_msg(e)); }
        }
    }

    fn update_path(&mut self, cd_arg: &str) {
        // Construir ruta resultante y normalizar.
        let mut buf = [0u8; MAX_PATH];
        let mut len;
        if cd_arg.as_bytes().first() == Some(&b'/') {
            buf[0] = b'/';
            len = 1;
        } else {
            // copiar actual
            let mut i = 0;
            while i < self.path_len && i < MAX_PATH {
                buf[i] = self.path[i];
                i += 1;
            }
            len = i;
            if len == 0 {
                buf[0] = b'/';
                len = 1;
            }
        }
        // recorrer componentes de cd_arg
        let bytes = cd_arg.as_bytes();
        let mut i = 0usize;
        if !bytes.is_empty() && bytes[0] == b'/' { i = 1; }
        while i < bytes.len() {
            while i < bytes.len() && bytes[i] == b'/' { i += 1; }
            if i >= bytes.len() { break; }
            let start = i;
            while i < bytes.len() && bytes[i] != b'/' { i += 1; }
            let comp = &bytes[start..i];
            if comp.is_empty() || comp == b"." {
            } else if comp == b".." {
                // subir un nivel
                if len > 1 {
                    // quitar barra final si hay
                    if buf[len - 1] == b'/' { len -= 1; }
                    while len > 1 && buf[len - 1] != b'/' { len -= 1; }
                    if len > 1 { len -= 1; } // quitar '/'
                }
                if len == 0 { buf[0] = b'/'; len = 1; }
            } else {
                if len > 1 || (len == 1 && buf[0] != b'/') {
                    if len < MAX_PATH { buf[len] = b'/'; len += 1; }
                } else if len == 1 && buf[0] == b'/' {
                    // raíz, no añadir barra extra
                } else if len == 0 {
                    buf[0] = b'/'; len = 1;
                }
                let mut j = 0;
                while j < comp.len() && len < MAX_PATH { buf[len] = comp[j]; len += 1; j += 1; }
            }
        }
        if len == 0 { buf[0] = b'/'; len = 1; }
        self.path = buf;
        self.path_len = len;
    }

    fn cmd_mkdir(&self, console: &mut Console, path: &str) {
        match fs::mkdir(&self.session.caps, self.slot(), self.session.cwd, path) {
            Ok(_) => { let _ = writeln!(console, "mkdir: creado '{}'", path); }
            Err(e) => { let _ = writeln!(console, "mkdir: {}", fs_err_msg(e)); }
        }
    }

    fn cmd_rmdir(&self, console: &mut Console, path: &str) {
        match fs::rmdir(&self.session.caps, self.slot(), self.session.cwd, path) {
            Ok(_) => { let _ = writeln!(console, "rmdir: eliminado '{}'", path); }
            Err(FsError::DirectoryNotEmpty) => {
                let _ = writeln!(console, "rmdir: error: el directorio no esta vacio: '{}'", path);
            }
            Err(e) => { let _ = writeln!(console, "rmdir: {}", fs_err_msg(e)); }
        }
    }

    fn cmd_rm(&self, console: &mut Console, path: &str) {
        match fs::rm(&self.session.caps, self.slot(), self.session.cwd, path) {
            Ok(_) => { let _ = writeln!(console, "rm: eliminado '{}'", path); }
            Err(e) => { let _ = writeln!(console, "rm: {}", fs_err_msg(e)); }
        }
    }

    fn cmd_rm_r(&self, console: &mut Console, path: &str) {
        match self.rm_any_recursive(self.session.cwd, path) {
            Ok(_) => { let _ = writeln!(console, "rm: eliminado recursivamente '{}'", path); }
            Err(e) => { let _ = writeln!(console, "rm -r: {}", fs_err_msg(e)); }
        }
    }

    fn rm_any_recursive(&self, cwd: NodeId, path: &str) -> Result<(), FsError> {
        match fs::rm(&self.session.caps, self.slot(), cwd, path) {
            Ok(()) => Ok(()),
            Err(FsError::NotAFile) => {
                let target = fs::cd(&self.session.caps, self.slot(), cwd, path)?;
                self.rm_dir_contents(target)?;
                fs::rmdir(&self.session.caps, self.slot(), cwd, path)
            }
            Err(e) => Err(e),
        }
    }

    fn rm_dir_contents(&self, dir: NodeId) -> Result<(), FsError> {
        // listar y eliminar cada hijo
        let mut entries = [DirEntry { attr: fs::NodeAttr { name: [0; MAX_NAME_LEN], name_len: 0, node_type: NodeType::File, size: 0 } }; MAX_LIST_ENTRIES];
        let count = fs::ls(&self.session.caps, self.slot(), dir, ".", &mut entries)?;
        let mut i = 0;
        while i < count {
            let e = &entries[i];
            // construir nombre como &str (nombres son ASCII en alpha)
            if let Ok(name) = core::str::from_utf8(e.attr.name_bytes()) {
                match e.attr.node_type {
                    NodeType::File => { fs::rm(&self.session.caps, self.slot(), dir, name)?; }
                    NodeType::Directory => {
                        // entrar, limpiar, salir implícito (operamos por dir id)
                        let child_dir = fs::cd(&self.session.caps, self.slot(), dir, name)?;
                        self.rm_dir_contents(child_dir)?;
                        fs::rmdir(&self.session.caps, self.slot(), dir, name)?;
                    }
                }
            }
            i += 1;
        }
        Ok(())
    }

    fn cmd_cat(&self, console: &mut Console, path: &str) {
        let mut buf = [0u8; 256];
        match fs::read(&self.session.caps, self.slot(), self.session.cwd, path, &mut buf) {
            Ok(n) => {
                let mut i = 0;
                while i < n {
                    let b = buf[i];
                    let byte = if b.is_ascii() { b } else { b'?' };
                    let s = core::str::from_utf8(core::slice::from_ref(&byte)).unwrap_or("?");
                    console.write_text(s);
                    i += 1;
                }
                if n == 0 || buf[n - 1] != b'\n' {
                    let _ = writeln!(console, "");
                }
            }
            Err(e) => { let _ = writeln!(console, "cat: {}", fs_err_msg(e)); }
        }
    }

    fn cmd_echo(&self, console: &mut Console, line: &str) {
        // formato: echo <texto> > <archivo>  (también soporta sin espacios alrededor de >)
        // quitar "echo"
        let rest = if line.len() >= 4 { trim_start(&line[4..]) } else { "" };
        // buscar '>' (última ocurrencia para permitir texto; simple: primera)
        if let Some(pos) = find_byte(rest, b'>') {
            let text = trim(&rest[..pos]);
            let file = trim(&rest[pos + 1..]);
            if file.is_empty() { let _ = writeln!(console, "echo: falta nombre de archivo"); return; }
            // quitar comillas simples opcionales del texto
            let content = strip_quotes(text);
            match fs::write(&self.session.caps, self.slot(), self.session.cwd, file, content.as_bytes()) {
                Ok(_) => { /* sin salida, como shell típico */ }
                Err(e) => { let _ = writeln!(console, "echo: {}", fs_err_msg(e)); }
            }
        } else {
            // echo sin redirección: imprimir texto
            let text = strip_quotes(trim(rest));
            let _ = writeln!(console, "{}", text);
        }
    }
}

fn fs_err_msg(e: FsError) -> &'static str {
    match e {
        FsError::Capability(_) => "permiso denegado",
        FsError::WrongObject => "objeto invalido",
        FsError::NotFound => "no encontrado",
        FsError::AlreadyExists => "ya existe",
        FsError::NotADirectory => "no es un directorio",
        FsError::NotAFile => "no es un archivo",
        FsError::DirectoryNotEmpty => "el directorio no esta vacio",
        FsError::InvalidName => "nombre invalido",
        FsError::NameTooLong => "nombre demasiado largo",
        FsError::PathTooLong => "ruta demasiado larga",
        FsError::NoSpace => "sin espacio",
        FsError::FileTooLarge => "archivo demasiado grande",
        FsError::IsRoot => "operacion invalida sobre la raiz",
        FsError::BufferTooSmall => "bufer insuficiente",
    }
}

fn trim(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && (bytes[start] == b' ' || bytes[start] == b'\t' || bytes[start] == b'\r' || bytes[start] == b'\n') { start += 1; }
    while end > start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t' || bytes[end - 1] == b'\r' || bytes[end - 1] == b'\n') { end -= 1; }
    &s[start..end]
}
fn trim_start(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0;
    while start < bytes.len() && (bytes[start] == b' ' || bytes[start] == b'\t') { start += 1; }
    &s[start..]
}
fn next_token(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut end = 0;
    while end < bytes.len() && bytes[end] != b' ' && bytes[end] != b'\t' { end += 1; }
    (&s[..end], &s[end..])
}
fn starts_with(s: &str, p: &str) -> bool {
    s.as_bytes().starts_with(p.as_bytes())
}
fn find_byte(s: &str, b: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() { if bytes[i] == b { return Some(i); } i += 1; }
    None
}
fn strip_quotes(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && ((b[0] == b'"' && b[b.len()-1] == b'"') || (b[0] == b'\'' && b[b.len()-1] == b'\'')) {
        &s[1..b.len()-1]
    } else { s }
}

/// Punto de entrada: intenta acceso sin login (denegado) y luego, si se provee
/// una sesión, no se usa aquí; la validación "sin login no se accede" se
/// demuestra explícitamente.
pub fn access_without_login(console: &mut Console, has_session: bool) -> bool {
    if !has_session {
        console.set_foreground(crate::console::WARN);
        let _ = writeln!(console, "terminal: acceso denegado (login requerido)");
        console.set_foreground(crate::console::TEXT);
        false
    } else {
        true
    }
}
