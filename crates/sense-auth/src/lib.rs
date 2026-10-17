//! OpenRouter sign-in via the PKCE key flow, with the key kept in the Keychain.
//!
//! The browser goes to `openrouter.ai/auth` and redirects to a one-shot
//! listener on 127.0.0.1; the code is then exchanged for a user-scoped key.
//! OpenRouter only accepts loopback or https callbacks for this flow, so a
//! custom URL scheme is not an option.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

const AUTH_URL: &str = "https://openrouter.ai/auth";
const EXCHANGE_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
const KEYCHAIN_SERVICE: &str = "dev.sense.openrouter";

/// `SENSE_KEYCHAIN_SERVICE` gives automated runs their own Keychain entry,
/// so signing in and out in tests never touches the user's credential.
fn keychain_service() -> String {
    std::env::var("SENSE_KEYCHAIN_SERVICE").unwrap_or_else(|_| KEYCHAIN_SERVICE.to_string())
}
const KEYCHAIN_ACCOUNT: &str = "api-key";
const KEY_LABEL: &str = "Sense";
/// Codes expire after 10 minutes server-side; stop listening at the same point.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct Account {
    pub api_key: String,
}

/// Returns the stored key without touching the network.
pub fn load() -> Option<Account> {
    keychain::get().map(|api_key| Account { api_key })
}

pub fn sign_out() -> Result<()> {
    keychain::delete()
}

/// Runs the whole browser round trip. Blocks until the user finishes or the
/// callback window closes, so call it off the UI thread.
pub fn sign_in(open_browser: impl FnOnce(&str) -> Result<()>) -> Result<Account> {
    let verifier = random_token()?;
    let state = random_token()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let listener = TcpListener::bind("127.0.0.1:0").context("bind callback listener")?;
    let callback = format!(
        "http://127.0.0.1:{}/callback",
        listener.local_addr()?.port()
    );

    let mut auth = Url::parse(AUTH_URL)?;
    auth.query_pairs_mut()
        .append_pair("callback_url", &callback)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("key_label", KEY_LABEL);
    open_browser(auth.as_str())?;

    let code = await_callback(&listener, &state)?;
    let api_key = exchange(&code, &verifier)?;
    keychain::set(&api_key)?;
    Ok(Account { api_key })
}

fn await_callback(listener: &TcpListener, expected_state: &str) -> Result<String> {
    let deadline = std::time::Instant::now() + CALLBACK_TIMEOUT;
    listener.set_nonblocking(true)?;
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    bail!("sign-in timed out");
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut request_line = String::new();
        BufReader::new(&stream).read_line(&mut request_line)?;
        // Browsers also ask for /favicon.ico; only /callback ends the flow.
        let Some(path) = request_line.split_whitespace().nth(1) else {
            continue;
        };
        let url = Url::parse(&format!("http://127.0.0.1{path}"))?;
        if url.path() != "/callback" {
            respond(&stream, "404 Not Found", "")?;
            continue;
        }
        let param = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        if param("state").as_deref() != Some(expected_state) {
            respond(
                &stream,
                "400 Bad Request",
                "Sign-in failed. Return to Sense and try again.",
            )?;
            bail!("sign-in callback had the wrong state");
        }
        let Some(code) = param("code") else {
            respond(
                &stream,
                "400 Bad Request",
                "Sign-in was cancelled. You can close this tab.",
            )?;
            bail!("sign-in was cancelled");
        };
        respond(
            &stream,
            "200 OK",
            "Signed in to Sense. You can close this tab.",
        )?;
        return Ok(code);
    }
}

fn respond(mut stream: &std::net::TcpStream, status: &str, message: &str) -> Result<()> {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Sense</title>\
         <body style=\"font:16px -apple-system,sans-serif;padding:48px\">{message}</body>"
    );
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}

fn exchange(code: &str, verifier: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct Response {
        key: String,
    }
    let response: Response = ureq::post(EXCHANGE_URL)
        .send_json(serde_json::json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256",
        }))
        .context("exchange sign-in code")?
        .body_mut()
        .read_json()
        .context("read sign-in exchange response")?;
    Ok(response.key)
}

fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| anyhow!("random: {err}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::{KEYCHAIN_ACCOUNT, keychain_service};
    use anyhow::Result;
    use security_framework::passwords;

    pub fn get() -> Option<String> {
        let bytes = passwords::get_generic_password(&keychain_service(), KEYCHAIN_ACCOUNT).ok()?;
        String::from_utf8(bytes).ok()
    }

    pub fn set(key: &str) -> Result<()> {
        passwords::set_generic_password(&keychain_service(), KEYCHAIN_ACCOUNT, key.as_bytes())?;
        Ok(())
    }

    pub fn delete() -> Result<()> {
        match passwords::delete_generic_password(&keychain_service(), KEYCHAIN_ACCOUNT) {
            // errSecItemNotFound: already signed out.
            Err(err) if err.code() == -25300 => Ok(()),
            other => Ok(other?),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    use anyhow::{Result, bail};

    pub fn get() -> Option<String> {
        None
    }

    pub fn set(_: &str) -> Result<()> {
        bail!("credential storage is only implemented on macOS")
    }

    pub fn delete() -> Result<()> {
        Ok(())
    }
}
