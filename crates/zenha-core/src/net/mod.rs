//! Network: token server HTTP client and reconnection policy.

pub(crate) mod backoff;
pub(crate) mod token;

/// Installs ring as the process-wide rustls provider; aws-lc aborts before `main` on a bad `OPENSSL_ia32cap`.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
