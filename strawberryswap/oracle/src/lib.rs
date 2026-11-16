//! StrawberrySwap's price-proof pipeline (Phase 2): the ETHUSD price endpoint, the KMS-backed notary and the relayer
//! that keeps `ZkPriceFeed` fresh. See docs/ORACLE.md for the design and the trust model.
//!
//! ```text
//! exchanges ─► price-endpoint ─TLS─► notary (KMS signs the body's commitment) ─► relayer: Groth16 proof,
//!                                                                                 ZkPriceFeed.submit every ~15 s
//! ```

#![forbid(unsafe_code)]

pub mod body;
pub mod chain;
pub mod cli;
pub mod commit;
pub mod endpoint;
pub mod kms;
pub mod notary;
pub mod notary_service;
pub mod prover;
pub mod relayer;
pub mod sources;

/// An error with its causes, `outer: inner: root` (an HTTP client's top-level message rarely names the cause).
pub fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.ends_with(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// Wall-clock Unix time, seconds (a clock before 1970 reads 0).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Layer>>);

    impl std::fmt::Display for Layer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|e| e as _)
        }
    }

    #[test]
    fn an_error_chain_names_every_cause_once() {
        let error = Layer("request failed", Some(Box::new(Layer("connect", Some(Box::new(Layer("refused", None)))))));
        assert_eq!(error_chain(&error), "request failed: connect: refused");
        let repeated = Layer("tls: bad certificate", Some(Box::new(Layer("bad certificate", None))));
        assert_eq!(error_chain(&repeated), "tls: bad certificate");
    }
}
