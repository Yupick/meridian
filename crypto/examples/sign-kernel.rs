//! Herramienta de desarrollo para firmar la imagen del kernel con Ed25519.
//!
//! Uso:
//!   cargo run -p meridian-crypto --example sign-kernel -- <kernel> <firma> [secreto-hex]
//!   cargo run -p meridian-crypto --example sign-kernel -- --public-key [secreto-hex]
//!
//! Si no se proporciona `secreto-hex` se usa una clave fija **solo de desarrollo**.
//! Nunca usar esa clave en una imagen de producción.

use meridian_crypto::ed25519::{
    secret_to_public, sign, PUBLIC_KEY_LEN, SECRET_KEY_LEN, SIGNATURE_LEN,
};
use std::{env, fs, process};

/// Clave de desarrollo derivada del vector RFC 8032 TEST 1. No es secreta.
const DEV_SECRET_HEX: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";

fn usage() -> ! {
    eprintln!("Uso: sign-kernel <kernel> <firma> [secreto-hex]");
    eprintln!("     sign-kernel --public-key [secreto-hex]");
    process::exit(2);
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_secret(hex: &str) -> [u8; SECRET_KEY_LEN] {
    if hex.len() != SECRET_KEY_LEN * 2 {
        eprintln!(
            "secreto hexadecimal inválido: se esperan {} caracteres, se recibieron {}",
            SECRET_KEY_LEN * 2,
            hex.len()
        );
        process::exit(2);
    }

    let mut secret = [0u8; SECRET_KEY_LEN];
    let bytes = hex.as_bytes();
    for (index, chunk) in bytes.chunks_exact(2).enumerate() {
        let high = nibble(chunk[0]).unwrap_or_else(|| {
            eprintln!("secreto hexadecimal inválido en la posición {}", index * 2);
            process::exit(2);
        });
        let low = nibble(chunk[1]).unwrap_or_else(|| {
            eprintln!(
                "secreto hexadecimal inválido en la posición {}",
                index * 2 + 1
            );
            process::exit(2);
        });
        secret[index] = (high << 4) | low;
    }
    secret
}

fn print_hex(bytes: &[u8]) {
    for byte in bytes {
        print!("{byte:02x}");
    }
    println!();
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }

    let public_key_mode = args[0] == "--public-key";
    if public_key_mode {
        args.remove(0);
    }

    let secret_hex = if public_key_mode {
        if args.len() > 1 {
            usage();
        }
        args.pop()
    } else {
        if args.len() != 2 && args.len() != 3 {
            usage();
        }
        if args.len() == 3 {
            args.pop()
        } else {
            None
        }
    };

    let secret_hex = secret_hex.unwrap_or_else(|| {
        eprintln!("AVISO: usando clave de firma Ed25519 SOLO DE DESARROLLO");
        DEV_SECRET_HEX.to_string()
    });
    let secret = parse_secret(&secret_hex);

    if public_key_mode {
        let public_key = secret_to_public(&secret);
        debug_assert_eq!(public_key.len(), PUBLIC_KEY_LEN);
        print_hex(&public_key);
        return;
    }

    let kernel_path = &args[0];
    let signature_path = &args[1];
    let kernel = fs::read(kernel_path).unwrap_or_else(|err| {
        eprintln!("no se pudo leer {kernel_path}: {err}");
        process::exit(1);
    });

    let signature = sign(&secret, &kernel);
    debug_assert_eq!(signature.len(), SIGNATURE_LEN);
    fs::write(signature_path, signature).unwrap_or_else(|err| {
        eprintln!("no se pudo escribir {signature_path}: {err}");
        process::exit(1);
    });

    println!(
        "[sign-kernel] {} bytes firmados -> {}",
        kernel.len(),
        signature_path
    );
    print!("[sign-kernel] firma: ");
    print_hex(&signature);
}
