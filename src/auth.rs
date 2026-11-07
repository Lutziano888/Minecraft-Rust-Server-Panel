//! Passwort, Sitzungen (Cookie) und Schutz gegen Raten und CSRF.

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

pub const COOKIE: &str = "hp_session";

/// Fehlversuche einer Adresse und bis wann sie gesperrt ist.
type Fails = (Vec<Instant>, Option<Instant>);
const SESSION_DAYS: u64 = 30;
const MAX_FAILS: usize = 5;
const FAIL_WINDOW: Duration = Duration::from_secs(60);
const LOCKOUT: Duration = Duration::from_secs(120);

pub fn hash_password(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).map_err(|e| e.to_string())?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("Zufallsgenerator");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Default)]
pub struct Auth {
    sessions: Mutex<HashMap<String, Instant>>,
    fails: Mutex<HashMap<IpAddr, Fails>>,
}

impl Auth {
    pub fn new_session(&self) -> String {
        let token = random_token();
        let mut s = self.sessions.lock().unwrap();
        let now = Instant::now();
        s.retain(|_, exp| *exp > now);
        s.insert(token.clone(), now + Duration::from_secs(SESSION_DAYS * 86400));
        token
    }

    pub fn valid(&self, token: &str) -> bool {
        let s = self.sessions.lock().unwrap();
        s.get(token).is_some_and(|exp| *exp > Instant::now())
    }

    pub fn end(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }

    pub fn end_all(&self) {
        self.sessions.lock().unwrap().clear();
    }

    /// Sekunden, die diese Adresse noch gesperrt ist.
    pub fn locked(&self, ip: IpAddr) -> Option<u64> {
        let f = self.fails.lock().unwrap();
        let (_, until) = f.get(&ip)?;
        let until = (*until)?;
        let now = Instant::now();
        (until > now).then(|| (until - now).as_secs() + 1)
    }

    pub fn failed(&self, ip: IpAddr) {
        let mut f = self.fails.lock().unwrap();
        let now = Instant::now();
        let entry = f.entry(ip).or_default();
        entry.0.retain(|t| now.duration_since(*t) < FAIL_WINDOW);
        entry.0.push(now);
        if entry.0.len() >= MAX_FAILS {
            entry.1 = Some(now + LOCKOUT);
            entry.0.clear();
        }
    }

    pub fn succeeded(&self, ip: IpAddr) {
        self.fails.lock().unwrap().remove(&ip);
    }
}

/// Den Sitzungs-Token aus dem Cookie-Header lesen.
pub fn token_from_cookies(header: Option<&str>) -> Option<String> {
    header?.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == COOKIE).then(|| v.to_owned())
    })
}

pub fn session_cookie(token: &str) -> String {
    format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_DAYS * 86400
    )
}

pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_roundtrip() {
        let h = hash_password("geheim").unwrap();
        assert!(verify_password(&h, "geheim"));
        assert!(!verify_password(&h, "falsch"));
    }

    #[test]
    fn cookie_parsing() {
        assert_eq!(
            token_from_cookies(Some("a=b; hp_session=xyz; c=d")),
            Some("xyz".into())
        );
        assert_eq!(token_from_cookies(Some("a=b")), None);
    }

    #[test]
    fn lockout_after_fails() {
        let a = Auth::default();
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        for _ in 0..MAX_FAILS {
            a.failed(ip);
        }
        assert!(a.locked(ip).is_some());
    }
}
