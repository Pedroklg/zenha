use zenha_protocol::{MAX_P2P_SIGNAL_BYTES, P2pSignal, P2pSignalError};

pub(super) fn encode_p2p_signal(signal: &P2pSignal) -> Result<Vec<u8>, P2pSignalError> {
    signal.validate()?;
    let encoded =
        serde_json::to_vec(signal).map_err(|err| P2pSignalError::InvalidJson(err.to_string()))?;
    if encoded.len() > MAX_P2P_SIGNAL_BYTES {
        return Err(P2pSignalError::TooLarge {
            actual: encoded.len(),
            max: MAX_P2P_SIGNAL_BYTES,
        });
    }
    Ok(encoded)
}

pub(super) fn decode_p2p_signal(payload: &[u8]) -> Result<P2pSignal, P2pSignalError> {
    if payload.is_empty() {
        return Err(P2pSignalError::Empty);
    }
    if payload.len() > MAX_P2P_SIGNAL_BYTES {
        return Err(P2pSignalError::TooLarge {
            actual: payload.len(),
            max: MAX_P2P_SIGNAL_BYTES,
        });
    }
    let signal: P2pSignal = serde_json::from_slice(payload)
        .map_err(|err| P2pSignalError::InvalidJson(err.to_string()))?;
    signal.validate()?;
    Ok(signal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_variants_round_trip_through_compact_json() {
        let samples = vec![
            P2pSignal::Request {
                share_epoch: 7,
                session_id: "sess-01".to_owned(),
            },
            P2pSignal::Reject {
                session_id: "sess-01".to_owned(),
                reason: "max_viewers".to_owned(),
            },
            P2pSignal::Offer {
                share_epoch: 7,
                session_id: "sess-01".to_owned(),
                sdp: "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n".to_owned(),
            },
            P2pSignal::Answer {
                session_id: "sess-01".to_owned(),
                sdp: "v=0\r\no=- 3 2 IN IP4 127.0.0.1\r\n".to_owned(),
            },
            P2pSignal::Ice {
                session_id: "sess-01".to_owned(),
                candidate: "candidate:1 1 udp 2122260223 192.168.1.2 50000 typ host".to_owned(),
                sdp_mid: "0".to_owned(),
                sdp_mline_index: 0,
            },
            P2pSignal::Fallback {
                session_id: "sess-01".to_owned(),
                reason: "ice_timeout".to_owned(),
            },
            P2pSignal::Bye {
                session_id: "sess-01".to_owned(),
            },
        ];

        for signal in samples {
            let encoded = encode_p2p_signal(&signal).unwrap();
            assert!(!encoded.contains(&b'\n'));
            let decoded = decode_p2p_signal(&encoded).unwrap();
            assert_eq!(decoded, signal);
            assert_eq!(decoded.session_id(), "sess-01");
        }
    }

    #[test]
    fn accepts_exact_size_limit_and_rejects_oversized_payloads() {
        let base = P2pSignal::Offer {
            share_epoch: 1,
            session_id: "s".to_owned(),
            sdp: "x".to_owned(),
        };
        let overhead = encode_p2p_signal(&base).unwrap().len() - 1;

        let exact = P2pSignal::Offer {
            share_epoch: 1,
            session_id: "s".to_owned(),
            sdp: "x".repeat(MAX_P2P_SIGNAL_BYTES - overhead),
        };
        let encoded = encode_p2p_signal(&exact).unwrap();
        assert_eq!(encoded.len(), MAX_P2P_SIGNAL_BYTES);
        assert_eq!(decode_p2p_signal(&encoded), Ok(exact));

        let too_large = P2pSignal::Offer {
            share_epoch: 1,
            session_id: "s".to_owned(),
            sdp: "x".repeat(MAX_P2P_SIGNAL_BYTES - overhead + 1),
        };
        assert_eq!(
            encode_p2p_signal(&too_large),
            Err(P2pSignalError::TooLarge {
                actual: MAX_P2P_SIGNAL_BYTES + 1,
                max: MAX_P2P_SIGNAL_BYTES,
            })
        );

        let oversized_raw = vec![b' '; MAX_P2P_SIGNAL_BYTES + 1];
        assert_eq!(
            decode_p2p_signal(&oversized_raw),
            Err(P2pSignalError::TooLarge {
                actual: MAX_P2P_SIGNAL_BYTES + 1,
                max: MAX_P2P_SIGNAL_BYTES,
            })
        );
    }

    #[test]
    fn rejects_empty_malformed_and_invalid_signals() {
        assert_eq!(decode_p2p_signal(&[]), Err(P2pSignalError::Empty));
        assert!(matches!(
            decode_p2p_signal(b"not-json"),
            Err(P2pSignalError::InvalidJson(_))
        ));
        assert!(matches!(
            decode_p2p_signal(br#"{"type":"unknown","session_id":"s"}"#),
            Err(P2pSignalError::InvalidJson(_))
        ));
        assert!(matches!(
            decode_p2p_signal(br#"{"type":"offer","session_id":"s"}"#),
            Err(P2pSignalError::InvalidJson(_))
        ));
        assert_eq!(
            decode_p2p_signal(br#"{"type":"bye","session_id":""}"#),
            Err(P2pSignalError::InvalidField("session_id"))
        );
        assert_eq!(
            decode_p2p_signal(br#"{"type":"offer","share_epoch":1,"session_id":"s","sdp":"  "}"#),
            Err(P2pSignalError::InvalidField("sdp"))
        );
        assert_eq!(
            decode_p2p_signal(
                br#"{"type":"ice","session_id":"s","candidate":"c","sdp_mid":"0","sdp_mline_index":-1}"#
            ),
            Err(P2pSignalError::InvalidField("sdp_mline_index"))
        );
        assert_eq!(
            encode_p2p_signal(&P2pSignal::Reject {
                session_id: "s".to_owned(),
                reason: "bad\nreason".to_owned(),
            }),
            Err(P2pSignalError::InvalidField("reason"))
        );
    }
}
