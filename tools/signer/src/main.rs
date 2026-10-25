//! Release signing for the in-app updater.
//!
//!   simple-desk-widgets-signer keygen <secret-key-file> <public-key-file>
//!   simple-desk-widgets-signer sign <secret-key-file> <file> <version>   → writes <file>.minisig
//!
//! The secret key stays on the release machine (outside the repo); the public
//! key is compiled into the app (src-tauri/updater.pub).

use minisign::{KeyPair, SecretKey, SecretKeyBox};
use std::{env, fs, io::Cursor, path::Path, process::ExitCode};

fn keygen(secret: &Path, public: &Path) -> Result<(), String> {
    if secret.exists() {
        return Err(format!("{} already exists; not overwriting a signing key", secret.display()));
    }
    let KeyPair { pk, sk } = KeyPair::generate_unencrypted_keypair().map_err(|e| e.to_string())?;
    if let Some(dir) = secret.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let sk_box = sk.to_box(None).map_err(|e| e.to_string())?;
    fs::write(secret, sk_box.to_string()).map_err(|e| e.to_string())?;
    // The app embeds only the key itself (the second line of the box).
    let pk_box = pk.to_box().map_err(|e| e.to_string())?.to_string();
    let pk_line = pk_box.lines().nth(1).ok_or("unexpected public key format")?;
    fs::write(public, format!("{pk_line}\n")).map_err(|e| e.to_string())?;
    println!("secret key: {}\npublic key: {}", secret.display(), public.display());
    Ok(())
}

/// The version goes into the signed comment, so the app can tell the exe is
/// the version the release says it is.
fn sign(secret: &Path, file: &Path, version: &str) -> Result<(), String> {
    let text = fs::read_to_string(secret).map_err(|e| format!("{}: {e}", secret.display()))?;
    let sk: SecretKey = SecretKeyBox::from_string(&text)
        .and_then(SecretKeyBox::into_unencrypted_secret_key)
        .map_err(|e| e.to_string())?;
    let data = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let sig = minisign::sign(None, &sk, Cursor::new(data), Some(&format!("file:{name} version:{version}")), None)
        .map_err(|e| e.to_string())?;
    let out = file.with_file_name(format!("{name}.minisig"));
    fs::write(&out, sig.to_string()).map_err(|e| e.to_string())?;
    println!("signature: {}", out.display());
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen", secret, public] => keygen(Path::new(secret), Path::new(public)),
        ["sign", secret, file, version] => sign(Path::new(secret), Path::new(file), version),
        _ => Err("usage: simple-desk-widgets-signer keygen <secret> <public> | sign <secret> <file> <version>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
