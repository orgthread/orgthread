//! The login of the door, for one person: an OAuth server, as Claude's connector
//! expects one, with a single password. Sessions live in memory, so a restart of
//! the service only means logging in once more.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CODE_LIFE: Duration = Duration::from_secs(60);
const ACCESS_LIFE: Duration = Duration::from_secs(3600);
const REFRESH_LIFE: Duration = Duration::from_secs(30 * 24 * 3600);
const LOCK_LIFE: Duration = Duration::from_secs(15 * 60);
const MAX_FAILS: usize = 5;
const MAX_CLIENTS: usize = 100;
/// Where Claude's apps ask to be sent back. No other place is allowed.
const REDIRECT: &str = "https://claude.ai/api/mcp/auth_callback";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn password_hash(password: &str) -> String {
    hex(&Sha256::digest(password.as_bytes()))
}

fn random() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}

/// The S256 challenge of a verifier: its SHA-256 in base64url with no padding.
pub fn challenge_of(verifier: &str) -> String {
    const SIGNS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let digest = Sha256::digest(verifier.as_bytes());
    let mut out = String::new();
    for chunk in digest.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(SIGNS[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    out
}

/// A form or query string as pairs, with `%XX` and `+` decoded.
pub fn parse_form(text: &str) -> HashMap<String, String> {
    let decode = |part: &str| {
        let bytes = part.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() && part.is_char_boundary(i + 3) => {
                    match u8::from_str_radix(&part[i + 1..i + 3], 16) {
                        Ok(b) => {
                            out.push(b);
                            i += 2;
                        }
                        Err(_) => out.push(b'%'),
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    text.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

struct Code {
    client: String,
    redirect: String,
    challenge: String,
    until: Instant,
}

#[derive(Default)]
struct Inner {
    clients: Vec<String>,
    codes: HashMap<String, Code>,
    access: HashMap<String, Instant>,
    refresh: HashMap<String, (String, Instant)>,
    fails: Vec<Instant>,
}

pub struct Login {
    base: String,
    hash: String,
    inner: Mutex<Inner>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    WrongPassword,
    Locked,
    BadRequest(String),
}

impl Login {
    pub fn new(base: &str, hash: &str) -> Self {
        Login {
            base: base.trim_end_matches('/').to_string(),
            hash: hash.to_string(),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The path part of the public address, which may hold a long secret. Everything
    /// lives under it, so without it a stranger finds no door and cannot cause a lock.
    pub fn prefix(&self) -> &str {
        let after = self.base.strip_prefix("https://").unwrap_or(&self.base);
        after.find('/').map_or("", |i| &after[i..])
    }

    pub fn resource(&self) -> String {
        format!("{}/mcp", self.base)
    }

    pub fn challenge_header(&self) -> String {
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
            self.base
        )
    }

    pub fn resource_metadata(&self) -> Value {
        json!({
            "resource": self.resource(),
            "authorization_servers": [self.base],
        })
    }

    pub fn server_metadata(&self) -> Value {
        json!({
            "issuer": self.base,
            "authorization_endpoint": format!("{}/authorize", self.base),
            "token_endpoint": format!("{}/token", self.base),
            "registration_endpoint": format!("{}/register", self.base),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
        })
    }

    /// A chat app names itself here. It only ever gets sent back to Claude.
    pub fn register(&self, body: &Value) -> Result<Value, String> {
        let uris = body["redirect_uris"].as_array().ok_or("no redirect_uris")?;
        if uris.is_empty() || uris.iter().any(|u| u != REDIRECT) {
            return Err("that redirect_uri is not allowed".to_string());
        }
        let id = random().map_err(|e| e.to_string())?;
        let mut inner = self.inner.lock().unwrap();
        if inner.clients.len() >= MAX_CLIENTS {
            inner.clients.remove(0);
        }
        inner.clients.push(id.clone());
        Ok(json!({
            "client_id": id,
            "redirect_uris": [REDIRECT],
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        }))
    }

    fn check(&self, p: &HashMap<String, String>) -> Result<(), String> {
        let get = |k: &str| p.get(k).map_or("", String::as_str);
        if !self
            .inner
            .lock()
            .unwrap()
            .clients
            .iter()
            .any(|c| c == get("client_id"))
        {
            return Err("this client is not known".to_string());
        }
        if get("redirect_uri") != REDIRECT {
            return Err("that redirect_uri is not allowed".to_string());
        }
        if get("response_type") != "code" {
            return Err("response_type must be code".to_string());
        }
        if get("code_challenge").is_empty() || get("code_challenge_method") != "S256" {
            return Err("S256 PKCE is required".to_string());
        }
        if !get("resource").is_empty() && get("resource") != self.resource() {
            return Err("that resource is not this server".to_string());
        }
        Ok(())
    }

    /// The page that asks for the password. It holds the request in hidden fields.
    pub fn page(&self, query: &str, message: &str) -> Result<String, String> {
        let p = parse_form(query);
        self.check(&p)?;
        let mut fields = String::new();
        for (k, v) in &p {
            if k != "password" {
                fields.push_str(&format!(
                    "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
                    escape(k),
                    escape(v)
                ));
            }
        }
        Ok(format!(
            "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
             <title>orgthread login</title><body style=\"font:16px sans-serif;margin:2em auto;max-width:24em;padding:0 1em\">\
             <h1 style=\"font-size:20px\">orgthread login</h1><p>{}</p>\
             <form method=\"post\" action=\"{}/authorize\">{fields}\
             <p><input name=\"password\" type=\"password\" autofocus autocomplete=\"current-password\" style=\"width:100%;font-size:16px;padding:.5em;box-sizing:border-box\"></p>\
             <p><button style=\"font-size:16px;padding:.5em 1em\">Allow Claude to use orgthread</button></p></form>",
            escape(message),
            escape(self.prefix())
        ))
    }

    /// The password came in. Gives the address to send the browser back to.
    pub fn approve(&self, form: &str) -> Result<String, Refused> {
        let p = parse_form(form);
        self.check(&p).map_err(Refused::BadRequest)?;
        let get = |k: &str| p.get(k).map_or("", String::as_str);
        let mut inner = self.inner.lock().unwrap();
        inner.fails.retain(|t| t.elapsed() < LOCK_LIFE);
        if inner.fails.len() >= MAX_FAILS {
            return Err(Refused::Locked);
        }
        if password_hash(get("password")) != self.hash {
            inner.fails.push(Instant::now());
            return Err(if inner.fails.len() >= MAX_FAILS {
                Refused::Locked
            } else {
                Refused::WrongPassword
            });
        }
        inner.fails.clear();
        let code = random().map_err(|e| Refused::BadRequest(e.to_string()))?;
        inner.codes.retain(|_, c| c.until > Instant::now());
        inner.codes.insert(
            code.clone(),
            Code {
                client: get("client_id").to_string(),
                redirect: get("redirect_uri").to_string(),
                challenge: get("code_challenge").to_string(),
                until: Instant::now() + CODE_LIFE,
            },
        );
        let mut back = format!("{REDIRECT}?code={}", encode(&code));
        if !get("state").is_empty() {
            back.push_str(&format!("&state={}", encode(get("state"))));
        }
        Ok(back)
    }

    fn tokens(&self, inner: &mut Inner, client: &str) -> Result<Value, &'static str> {
        let (access, refresh) = match (random(), random()) {
            (Ok(a), Ok(r)) => (a, r),
            _ => return Err("server_error"),
        };
        let now = Instant::now();
        inner.access.retain(|_, until| *until > now);
        inner.refresh.retain(|_, (_, until)| *until > now);
        inner.access.insert(access.clone(), now + ACCESS_LIFE);
        inner
            .refresh
            .insert(refresh.clone(), (client.to_string(), now + REFRESH_LIFE));
        Ok(json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": ACCESS_LIFE.as_secs(),
            "refresh_token": refresh,
        }))
    }

    /// The token endpoint. An error is the OAuth error word.
    pub fn token(&self, form: &str) -> Result<Value, &'static str> {
        let p = parse_form(form);
        let get = |k: &str| p.get(k).map_or("", String::as_str);
        if !get("resource").is_empty() && get("resource") != self.resource() {
            return Err("invalid_target");
        }
        let mut inner = self.inner.lock().unwrap();
        match get("grant_type") {
            "authorization_code" => {
                // A code works once, also when the attempt fails.
                let code = inner.codes.remove(get("code")).ok_or("invalid_grant")?;
                let good = code.until > Instant::now()
                    && code.client == get("client_id")
                    && code.redirect == get("redirect_uri")
                    && code.challenge == challenge_of(get("code_verifier"));
                if !good {
                    return Err("invalid_grant");
                }
                self.tokens(&mut inner, &code.client)
            }
            "refresh_token" => {
                let (client, until) = inner
                    .refresh
                    .remove(get("refresh_token"))
                    .ok_or("invalid_grant")?;
                if until <= Instant::now() || client != get("client_id") {
                    return Err("invalid_grant");
                }
                self.tokens(&mut inner, &client)
            }
            _ => Err("unsupported_grant_type"),
        }
    }

    pub fn allows(&self, token: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .access
            .get(token)
            .is_some_and(|until| *until > Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> Login {
        Login::new("https://door.test/", &password_hash("right password"))
    }

    fn client(l: &Login) -> String {
        let r = l.register(&json!({ "redirect_uris": [REDIRECT] })).unwrap();
        r["client_id"].as_str().unwrap().to_string()
    }

    fn request(client: &str, verifier: &str) -> String {
        format!(
            "client_id={client}&redirect_uri={}&response_type=code&state=s1&code_challenge={}&code_challenge_method=S256&resource={}",
            encode(REDIRECT),
            challenge_of(verifier),
            encode("https://door.test/mcp")
        )
    }

    fn code_from(back: &str) -> String {
        parse_form(back.split_once('?').unwrap().1)["code"].clone()
    }

    #[test]
    fn the_challenge_matches_the_example_of_the_standard() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn forms_decode() {
        let p = parse_form("a=1+2&b=%C3%BC%26&c");
        assert_eq!(p["a"], "1 2");
        assert_eq!(p["b"], "ü&");
        assert_eq!(p["c"], "");
    }

    #[test]
    fn the_whole_login_gives_a_token_that_works() {
        let l = login();
        let c = client(&l);
        assert!(!l.allows("anything"));
        let page = l.page(&request(&c, "v1"), "Password").unwrap();
        assert!(page.contains("type=\"password\""));
        let back = l
            .approve(&format!("{}&password=right+password", request(&c, "v1")))
            .unwrap();
        assert!(back.starts_with(REDIRECT));
        assert!(back.contains("state=s1"));
        let form = format!(
            "grant_type=authorization_code&code={}&client_id={c}&redirect_uri={}&code_verifier=v1&resource={}",
            code_from(&back),
            encode(REDIRECT),
            encode("https://door.test/mcp")
        );
        let t = l.token(&form).unwrap();
        assert!(l.allows(t["access_token"].as_str().unwrap()));
        assert_eq!(l.token(&form), Err("invalid_grant"), "a code works once");

        // A refresh gives a new pair, and the old refresh token is dead.
        let old = t["refresh_token"].as_str().unwrap();
        let again = format!("grant_type=refresh_token&refresh_token={old}&client_id={c}");
        let t2 = l.token(&again).unwrap();
        assert!(l.allows(t2["access_token"].as_str().unwrap()));
        assert_eq!(l.token(&again), Err("invalid_grant"));
    }

    #[test]
    fn a_wrong_verifier_gets_nothing() {
        let l = login();
        let c = client(&l);
        let back = l
            .approve(&format!("{}&password=right+password", request(&c, "v1")))
            .unwrap();
        let form = format!(
            "grant_type=authorization_code&code={}&client_id={c}&redirect_uri={}&code_verifier=wrong",
            code_from(&back),
            encode(REDIRECT)
        );
        assert_eq!(l.token(&form), Err("invalid_grant"));
    }

    #[test]
    fn five_wrong_passwords_lock_the_door_even_for_the_right_one() {
        let l = login();
        let c = client(&l);
        let form = |pw: &str| format!("{}&password={pw}", request(&c, "v1"));
        for _ in 0..4 {
            assert_eq!(l.approve(&form("wrong")), Err(Refused::WrongPassword));
        }
        assert_eq!(l.approve(&form("wrong")), Err(Refused::Locked));
        assert_eq!(l.approve(&form("right+password")), Err(Refused::Locked));
    }

    #[test]
    fn a_right_password_clears_the_count() {
        let l = login();
        let c = client(&l);
        let form = |pw: &str| format!("{}&password={pw}", request(&c, "v1"));
        for _ in 0..4 {
            let _ = l.approve(&form("wrong"));
        }
        assert!(l.approve(&form("right+password")).is_ok());
        assert_eq!(l.approve(&form("wrong")), Err(Refused::WrongPassword));
    }

    #[test]
    fn only_claude_may_be_sent_back_to() {
        let l = login();
        let evil = json!({ "redirect_uris": ["https://evil.test/cb"] });
        assert!(l.register(&evil).is_err());
        let c = client(&l);
        let q = request(&c, "v1").replace(&encode(REDIRECT), &encode("https://evil.test/cb"));
        assert!(l.page(&q, "x").is_err());
        assert!(matches!(
            l.approve(&format!("{q}&password=right+password")),
            Err(Refused::BadRequest(_))
        ));
    }

    #[test]
    fn plain_pkce_and_unknown_clients_are_refused() {
        let l = login();
        let c = client(&l);
        let plain =
            request(&c, "v1").replace("code_challenge_method=S256", "code_challenge_method=plain");
        assert!(l.page(&plain, "x").is_err());
        assert!(l.page(&request("unknown", "v1"), "x").is_err());
    }

    #[test]
    fn the_page_escapes_what_it_is_given() {
        let l = login();
        let c = client(&l);
        let q = format!(
            "{}&state=%22%3E%3Cscript%3E",
            request(&c, "v1").replace("&state=s1", "")
        );
        let page = l.page(&q, "x").unwrap();
        assert!(!page.contains("<script>"));
    }

    #[test]
    fn a_secret_in_the_address_is_the_prefix() {
        let l = Login::new("https://door.test/s3cret/", "x");
        assert_eq!(l.prefix(), "/s3cret");
        assert_eq!(l.resource(), "https://door.test/s3cret/mcp");
        assert_eq!(l.server_metadata()["issuer"], "https://door.test/s3cret");
        assert_eq!(login().prefix(), "");
    }

    #[test]
    fn the_login_form_posts_under_the_secret() {
        let l = Login::new("https://door.test/s3cret", &password_hash("p"));
        let c = client(&l);
        let q = request(&c, "v").replace(
            &encode("https://door.test/mcp"),
            &encode("https://door.test/s3cret/mcp"),
        );
        assert!(
            l.page(&q, "x")
                .unwrap()
                .contains("action=\"/s3cret/authorize\"")
        );
    }

    #[test]
    fn the_metadata_says_what_claude_needs() {
        let l = login();
        assert_eq!(l.resource_metadata()["resource"], "https://door.test/mcp");
        assert_eq!(
            l.resource_metadata()["authorization_servers"][0],
            "https://door.test"
        );
        assert_eq!(
            l.server_metadata()["code_challenge_methods_supported"][0],
            "S256"
        );
        assert!(l.challenge_header().contains("oauth-protected-resource"));
    }
}
