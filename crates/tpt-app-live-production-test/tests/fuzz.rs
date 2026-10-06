//! Deterministic fuzzing (spec 21.5) for the three untrusted-input
//! boundaries:
//!
//! 1. the show-file parser,
//! 2. inbound control-surface (OSC) message parsing,
//! 3. the local API request handling.
//!
//! Runs a seeded, mutating corpus in plain `cargo test` (deterministic,
//! any platform). `fuzz/` at the repo root carries the cargo-fuzz targets
//! for continuous coverage on Linux CI; any crash found there should
//! become a seed here.
//!
//! Contract under fuzz: parsers may reject (Err) but must never panic,
//! hang, or allocate unboundedly.

use tpt_app_live_production_model::showfile::ShowFile;
use tpt_app_live_production_test::Rng;

fn seeds_showfile() -> Vec<String> {
    vec![
        "schema_version = 1\nname = \"x\"".to_string(),
        std::fs::read_to_string(
            tpt_app_live_production_test::repo_root().join("shows/examples/golden-demo.tptshow"),
        )
        .expect("golden show exists"),
        "schema_version = 1\nname = \"x\"\n[[cues]]\nnumber = 1".to_string(),
        "schema_version = 1\nsources = [{ id = 1 }]".to_string(),
    ]
}

#[test]
fn fuzz_showfile_parser_never_panics() {
    let mut rng = Rng::new(0x5EED_0001);
    let mut corpus: Vec<Vec<u8>> = seeds_showfile()
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();
    for round in 0..20_000u32 {
        let seed_input = &corpus[(round as usize) % corpus.len()];
        let input = if (round as usize) < corpus.len() {
            seed_input.clone()
        } else {
            rng.mutate(seed_input)
        };
        // May error; must not panic.
        if let Ok(file) = ShowFile::from_str(&String::from_utf8_lossy(&input)) {
            // If it parses, domain conversion and validation must also be
            // total.
            if let Ok(show) = tpt_app_live_production_model::Show::try_from(file) {
                let _ = tpt_app_live_production_core::headless::validate_show(&show);
            }
        }
        if round % 5_000 == 0 {
            corpus.push(input);
        }
    }
}

#[test]
fn fuzz_osc_message_parser_never_panics() {
    use tpt_av_control_osc::OscServer;
    let mut rng = Rng::new(0x5EED_0002);
    let mut corpus: Vec<Vec<u8>> = vec![
        {
            let msg = tpt_av_control_osc::OscMessage::new("/go", &[]).unwrap();
            msg.encode()
        },
        {
            let msg = tpt_av_control_osc::OscMessage::new(
                "/cam/1",
                &[tpt_av_control_osc::OscArg::Float(0.5)],
            )
            .unwrap();
            msg.encode()
        },
        b"/go".to_vec(),
    ];
    for round in 0..20_000u32 {
        let input = if (round as usize) < corpus.len() {
            corpus[round as usize].clone()
        } else {
            rng.mutate(&corpus[(round as usize) % corpus.len()])
        };
        // Parse via the public byte-parse API: errors allowed, panics not.
        let _ = OscServer::parse_bytes(&input);
        if round % 5_000 == 0 {
            corpus.push(input);
        }
    }
}

#[test]
fn fuzz_local_api_request_handling_never_panics_or_hangs() {
    use std::io::{Read, Write};
    use std::net::{IpAddr, SocketAddr, TcpStream};
    use std::sync::{Arc, Mutex};

    let engine = {
        let show = {
            let file = ShowFile::load(
                tpt_app_live_production_test::repo_root()
                    .join("shows/examples/golden-demo.tptshow"),
            )
            .unwrap();
            tpt_app_live_production_model::Show::try_from(file).unwrap()
        };
        Arc::new(Mutex::new(
            tpt_app_live_production_core::engine::LiveEngine::build(
                show,
                tpt_app_live_production_core::engine::EngineConfig::default(),
            )
            .unwrap(),
        ))
    };

    let cfg = tpt_app_live_production_core::api::ApiConfig {
        enabled: true,
        // Port 0 = OS-assigned, loopback only.
        bind: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0),
        ..tpt_app_live_production_core::api::ApiConfig::default()
    };
    let mut api = tpt_app_live_production_core::api::spawn(engine, cfg).expect("api spawns");
    let addr = api.local_addr();

    let mut rng = Rng::new(0x5EED_0003);
    let valid = b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".to_vec();
    let mut corpus: Vec<Vec<u8>> = vec![
        valid.clone(),
        b"POST /show/cue/next HTTP/1.1\r\nContent-Length: 0\r\n\r\n".to_vec(),
        b"GET /show/state HTTP/1.1\r\n\r\n".to_vec(),
    ];

    for round in 0..2_000u32 {
        let input = if (round as usize) < corpus.len() {
            corpus[round as usize].clone()
        } else {
            rng.mutate(&corpus[(round as usize) % corpus.len()])
        };
        // Malformed requests may fail at the connection level; the server
        // must survive every one of them.
        if let Ok(mut stream) = TcpStream::connect(addr) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream.write_all(&input);
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf);
        }
        if round % 500 == 0 {
            corpus.push(input);
        }
    }

    // The server is still healthy after the barrage.
    let mut stream = TcpStream::connect(addr).expect("server alive after fuzz");
    stream.write_all(&valid).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "health check after fuzz: {response}"
    );
    api.shutdown();
}

use std::time::Duration;
