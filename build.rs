//! ビルド時に GoogleSQL の prebuilt WebAssembly モジュールを用意する。
//!
//! 取得元の優先順位:
//! 1. 環境変数 `GOOGLESQL_WASM` が指すローカルファイル(オフライン/開発用)
//! 2. `OUT_DIR` にキャッシュ済みの検証済みコピー
//! 3. GitHub Release からのダウンロード(`curl`)
//!
//! いずれの場合も SHA256 をピン留め値と照合してから採用する。

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// 同梱する googlesql.wasm のバージョンと検証情報(goccy/googlesql-wasm)。
const WASM_VERSION: &str = "v0.3.4";
const WASM_SHA256: &str = "5f14b3a74a9bb4e333b03e8420b11b633a1b77379053f02e44235abed08ae407";
const WASM_URL: &str =
    "https://github.com/goccy/googlesql-wasm/releases/download/v0.3.4/googlesql.wasm";

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=GOOGLESQL_WASM");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let dest = out_dir.join("googlesql.wasm");

    let bytes = resolve_wasm_bytes(&dest)?;
    verify_sha256(&bytes)?;
    fs::write(&dest, &bytes)?;

    // wasm の絶対パスを実行時参照できるよう公開する。
    println!("cargo::rustc-env=GOOGLESQL_WASM_PATH={}", dest.display());
    Ok(())
}

/// 優先順位に従って wasm のバイト列を取得する(検証は呼び出し側で行う)。
fn resolve_wasm_bytes(dest: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    if let Ok(local) = env::var("GOOGLESQL_WASM") {
        let bytes =
            fs::read(&local).map_err(|e| format!("GOOGLESQL_WASM={local} を読めません: {e}"))?;
        return Ok(bytes);
    }

    if let Ok(cached) = fs::read(dest)
        && sha256_hex(&cached) == WASM_SHA256
    {
        return Ok(cached);
    }

    download_with_curl(WASM_URL, dest)?;
    let bytes = fs::read(dest).map_err(|e| format!("ダウンロード済み wasm を読めません: {e}"))?;
    Ok(bytes)
}

/// `curl` で URL を `dest` にダウンロードする。
fn download_with_curl(url: &str, dest: &Path) -> Result<(), Box<dyn Error>> {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("curl を起動できません({WASM_VERSION}): {e}"))?;
    if !status.success() {
        return Err(format!("curl による {url} のダウンロードに失敗しました").into());
    }
    Ok(())
}

/// バイト列の SHA256 がピン留め値と一致することを検証する。
fn verify_sha256(bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let actual = sha256_hex(bytes);
    if actual != WASM_SHA256 {
        return Err(
            format!("googlesql.wasm の SHA256 不一致 (期待 {WASM_SHA256}, 実際 {actual})").into(),
        );
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
