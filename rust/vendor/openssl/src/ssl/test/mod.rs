#![allow(unused_imports)]

use std::env;
use std::fs::File;
use std::io::prelude::*;
use std::io::{self, BufReader};
use std::iter;
use std::mem;
use std::net::UdpSocket;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::dh::Dh;
use crate::error::ErrorStack;
use crate::hash::MessageDigest;
#[cfg(not(any(boringssl, awslc)))]
use crate::ocsp::{OcspResponse, OcspResponseStatus};
use crate::pkey::{Id, PKey};
use crate::srtp::SrtpProfileId;
use crate::ssl::test::server::Server;
#[cfg(any(ossl110, libressl))]
use crate::ssl::SslVersion;
use crate::ssl::{self, NameType, SslConnectorBuilder};
#[cfg(ossl111)]
use crate::ssl::{ClientHelloResponse, ExtensionContext};
use crate::ssl::{
    Error, HandshakeError, MidHandshakeSslStream, ShutdownResult, ShutdownState, Ssl, SslAcceptor,
    SslAcceptorBuilder, SslConnector, SslContext, SslContextBuilder, SslFiletype, SslMethod,
    SslOptions, SslSessionCacheMode, SslStream, SslVerifyMode, StatusType,
};
#[cfg(ossl110)]
use crate::x509::store::X509StoreBuilder;
#[cfg(ossl110)]
use crate::x509::verify::X509CheckFlags;
use crate::x509::{X509Name, X509StoreContext, X509VerifyResult, X509};

mod server;

static ROOT_CERT: &[u8] = include_bytes!("../../../test/root-ca.pem");
static CERT: &[u8] = include_bytes!("../../../test/cert.pem");
static KEY: &[u8] = include_bytes!("../../../test/key.pem");

#[test]
fn verify_untrusted() {
    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    client.ctx().set_verify(SslVerifyMode::PEER);

    client.connect_err();
}

#[test]
fn verify_trusted() {
    let server = Server::builder().build();

    let mut client = server.client();
    client.ctx().set_ca_file("test/root-ca.pem").unwrap();

    client.connect();
}

#[test]
#[cfg(ossl110)]
fn verify_trusted_with_set_cert() {
    let server = Server::builder().build();

    let mut store = X509StoreBuilder::new().unwrap();
    let x509 = X509::from_pem(ROOT_CERT).unwrap();
    store.add_cert(x509).unwrap();

    let mut client = server.client();
    client.ctx().set_verify(SslVerifyMode::PEER);
    client.ctx().set_verify_cert_store(store.build()).unwrap();

    client.connect();
}

#[test]
fn verify_untrusted_callback_override_ok() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let server = Server::builder().build();

    let mut client = server.client();
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, |_, x509| {
            CALLED_BACK.store(true, Ordering::SeqCst);
            assert!(x509.current_cert().is_some());
            true
        });

    client.connect();
    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn verify_untrusted_callback_override_bad() {
    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, |_, _| false);

    client.connect_err();
}

#[test]
fn verify_trusted_callback_override_ok() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let server = Server::builder().build();

    let mut client = server.client();
    client.ctx().set_ca_file("test/root-ca.pem").unwrap();
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, |_, x509| {
            CALLED_BACK.store(true, Ordering::SeqCst);
            assert!(x509.current_cert().is_some());
            true
        });

    client.connect();
    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn verify_trusted_callback_override_bad() {
    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    client.ctx().set_ca_file("test/root-ca.pem").unwrap();
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, |_, _| false);

    client.connect_err();
}

#[test]
fn verify_callback_load_certs() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let server = Server::builder().build();

    let mut client = server.client();
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, |_, x509| {
            CALLED_BACK.store(true, Ordering::SeqCst);
            assert!(x509.current_cert().is_some());
            true
        });

    client.connect();
    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn verify_trusted_get_error_ok() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let server = Server::builder().build();

    let mut client = server.client();
    client.ctx().set_ca_file("test/root-ca.pem").unwrap();
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, |_, x509| {
            CALLED_BACK.store(true, Ordering::SeqCst);
            assert_eq!(x509.error(), X509VerifyResult::OK);
            true
        });

    client.connect();
    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn verify_trusted_get_error_err() {
    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, |_, x509| {
            assert_ne!(x509.error(), X509VerifyResult::OK);
            false
        });

    client.connect_err();
}

#[test]
fn verify_callback() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let server = Server::builder().build();

    let mut client = server.client();
    let expected = "59172d9313e84459bcff27f967e79e6e9217e584";
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, move |_, x509| {
            CALLED_BACK.store(true, Ordering::SeqCst);
            let cert = x509.current_cert().unwrap();
            let digest = cert.digest(MessageDigest::sha1()).unwrap();
            assert_eq!(hex::encode(digest), expected);
            true
        });

    client.connect();
    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn ssl_verify_callback() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let server = Server::builder().build();

    let mut client = server.client().build().builder();
    let expected = "59172d9313e84459bcff27f967e79e6e9217e584";
    client
        .ssl()
        .set_verify_callback(SslVerifyMode::PEER, move |_, x509| {
            CALLED_BACK.store(true, Ordering::SeqCst);
            let cert = x509.current_cert().unwrap();
            let digest = cert.digest(MessageDigest::sha1()).unwrap();
            assert_eq!(hex::encode(digest), expected);
            true
        });

    client.connect();
    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn get_ctx_options() {
    let ctx = SslContext::builder(SslMethod::tls()).unwrap();
    ctx.options();
}

#[test]
fn set_ctx_options() {
    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    let opts = ctx.set_options(SslOptions::NO_TICKET);
    assert!(opts.contains(SslOptions::NO_TICKET));
}

#[test]
#[cfg(not(any(boringssl, awslc)))]
fn clear_ctx_options() {
    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    ctx.set_options(SslOptions::ALL);
    let opts = ctx.clear_options(SslOptions::ALL);
    assert!(!opts.contains(SslOptions::ALL));
}

#[test]
fn zero_length_buffers() {
    let server = Server::builder().build();

    let mut s = server.client().connect();
    assert_eq!(s.write(&[]).unwrap(), 0);
    assert_eq!(s.read(&mut []).unwrap(), 0);
}

#[test]
fn peer_certificate() {
    let server = Server::builder().build();

    let s = server.client().connect();
    let cert = s.ssl().peer_certificate().unwrap();
    let fingerprint = cert.digest(MessageDigest::sha1()).unwrap();
    assert_eq!(
        hex::encode(fingerprint),
        "59172d9313e84459bcff27f967e79e6e9217e584"
    );
}

#[test]
fn pending() {
    let mut server = Server::builder();
    server.io_cb(|mut s| s.write_all(&[0; 10]).unwrap());
    let server = server.build();

    let mut s = server.client().connect();
    s.read_exact(&mut [0]).unwrap();

    assert_eq!(s.ssl().pending(), 9);
    assert_eq!(s.read(&mut [0; 10]).unwrap(), 9);
}

#[test]
fn state() {
    const EXPECTED_STATE_STRING_LONG: &str = "SSL negotiation finished successfully";

    let server = Server::builder().build();

    let s = server.client().connect();
    #[cfg(not(any(boringssl, awslc)))]
    assert_eq!(s.ssl().state_string().trim(), "SSLOK");
    #[cfg(boringssl)]
    assert_eq!(s.ssl().state_string(), "!!!!!!");
    #[cfg(awslc)]
    assert_eq!(s.ssl().state_string(), EXPECTED_STATE_STRING_LONG);

    assert_eq!(s.ssl().state_string_long(), EXPECTED_STATE_STRING_LONG);
}

// when a connection uses ECDHE P-384 key exchange, then the temp key APIs
// return P-384 keys, and the peer and local keys are different.
#[test]
#[cfg(ossl300)]
fn peer_tmp_key_p384() {
    let mut server = Server::builder();
    server.ctx().set_groups_list("P-384").unwrap();
    let server = server.build();
    let s = server.client().connect();
    let peer_temp = s.ssl().peer_tmp_key().unwrap();
    assert_eq!(peer_temp.id(), Id::EC);
    assert_eq!(peer_temp.bits(), 384);

    let local_temp = s.ssl().tmp_key().unwrap();
    assert_eq!(local_temp.id(), Id::EC);
    assert_eq!(local_temp.bits(), 384);

    assert_ne!(
        peer_temp.ec_key().unwrap().public_key_to_der().unwrap(),
        local_temp.ec_key().unwrap().public_key_to_der().unwrap(),
    );
}

// when a connection uses RSA key exchange, then the peer (server) temp key is
// an Error because there is no temp key, and the local (client) temp key is the
// temp key sent in the initial key share.
#[test]
#[cfg(ossl300)]
fn peer_tmp_key_rsa() {
    let mut server = Server::builder();
    server.ctx().set_cipher_list("RSA").unwrap();
    // RSA key exchange is not allowed in TLS 1.3, so force the connection
    // to negotiate TLS 1.2
    server
        .ctx()
        .set_max_proto_version(Some(SslVersion::TLS1_2))
        .unwrap();
    let server = server.build();
    let mut client = server.client();
    client.ctx().set_groups_list("P-521").unwrap();
    let s = client.connect();
    let peer_temp = s.ssl().peer_tmp_key();
    assert!(peer_temp.is_err());

    // this is the temp key that the client sent in the initial key share
    let local_temp = s.ssl().tmp_key().unwrap();
    assert_eq!(local_temp.id(), Id::EC);
    assert_eq!(local_temp.bits(), 521);
}

/// Tests that when both the client as well as the server use SRTP and their
/// lists of supported protocols have an overlap -- with only ONE protocol
/// being valid for both.
#[test]
fn test_connect_with_srtp_ctx() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let guard = thread::spawn(move || {
        let stream = listener.accept().unwrap().0;
        let mut ctx = SslContext::builder(SslMethod::dtls()).unwrap();
        ctx.set_tlsext_use_srtp("SRTP_AES128_CM_SHA1_80:SRTP_AES128_CM_SHA1_32")
            .unwrap();
        ctx.set_certificate_file(Path::new("test/cert.pem"), SslFiletype::PEM)
            .unwrap();
        ctx.set_private_key_file(Path::new("test/key.pem"), SslFiletype::PEM)
            .unwrap();
        let mut ssl = Ssl::new(&ctx.build()).unwrap();
        ssl.set_mtu(1500).unwrap();
        let mut stream = ssl.accept(stream).unwrap();

        let mut buf = [0; 60];
        stream
            .ssl()
            .export_keying_material(&mut buf, "EXTRACTOR-dtls_srtp", None)
            .unwrap();

        stream.write_all(&[0]).unwrap();

        buf
    });

    let stream = TcpStream::connect(addr).unwrap();
    let mut ctx = SslContext::builder(SslMethod::dtls()).unwrap();
    ctx.set_tlsext_use_srtp("SRTP_AES128_CM_SHA1_80:SRTP_AES128_CM_SHA1_32")
        .unwrap();
    let mut ssl = Ssl::new(&ctx.build()).unwrap();
    ssl.set_mtu(1500).unwrap();
    let mut stream = ssl.connect(stream).unwrap();

    let mut buf = [1; 60];
    {
        let srtp_profile = stream.ssl().selected_srtp_profile().unwrap();
        assert_eq!("SRTP_AES128_CM_SHA1_80", srtp_profile.name());
        assert_eq!(SrtpProfileId::SRTP_AES128_CM_SHA1_80, srtp_profile.id());
    }
    stream
        .ssl()
        .export_keying_material(&mut buf, "EXTRACTOR-dtls_srtp", None)
        .expect("extract");

    stream.read_exact(&mut [0]).unwrap();

    let buf2 = guard.join().unwrap();

    assert_eq!(buf[..], buf2[..]);
}

/// Tests that when both the client as well as the server use SRTP and their
/// lists of supported protocols have an overlap -- with only ONE protocol
/// being valid for both.
#[test]
fn test_connect_with_srtp_ssl() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let guard = thread::spawn(move || {
        let stream = listener.accept().unwrap().0;
        let mut ctx = SslContext::builder(SslMethod::dtls()).unwrap();
        ctx.set_certificate_file(Path::new("test/cert.pem"), SslFiletype::PEM)
            .unwrap();
        ctx.set_private_key_file(Path::new("test/key.pem"), SslFiletype::PEM)
            .unwrap();
        let mut ssl = Ssl::new(&ctx.build()).unwrap();
        ssl.set_tlsext_use_srtp("SRTP_AES128_CM_SHA1_80:SRTP_AES128_CM_SHA1_32")
            .unwrap();
        let mut profilenames = String::new();
        for profile in ssl.srtp_profiles().unwrap() {
            if !profilenames.is_empty() {
                profilenames.push(':');
            }
            profilenames += profile.name();
        }
        assert_eq!(
            "SRTP_AES128_CM_SHA1_80:SRTP_AES128_CM_SHA1_32",
            profilenames
        );
        ssl.set_mtu(1500).unwrap();
        let mut stream = ssl.accept(stream).unwrap();

        let mut buf = [0; 60];
        stream
            .ssl()
            .export_keying_material(&mut buf, "EXTRACTOR-dtls_srtp", None)
            .unwrap();

        stream.write_all(&[0]).unwrap();

        buf
    });

    let stream = TcpStream::connect(addr).unwrap();
    let ctx = SslContext::builder(SslMethod::dtls()).unwrap();
    let mut ssl = Ssl::new(&ctx.build()).unwrap();
    ssl.set_tlsext_use_srtp("SRTP_AES128_CM_SHA1_80:SRTP_AES128_CM_SHA1_32")
        .unwrap();
    ssl.set_mtu(1500).unwrap();
    let mut stream = ssl.connect(stream).unwrap();

    let mut buf = [1; 60];
    {
        let srtp_profile = stream.ssl().selected_srtp_profile().unwrap();
        assert_eq!("SRTP_AES128_CM_SHA1_80", srtp_profile.name());
        assert_eq!(SrtpProfileId::SRTP_AES128_CM_SHA1_80, srtp_profile.id());
    }
    stream
        .ssl()
        .export_keying_material(&mut buf, "EXTRACTOR-dtls_srtp", None)
        .expect("extract");

    stream.read_exact(&mut [0]).unwrap();

    let buf2 = guard.join().unwrap();

    assert_eq!(buf[..], buf2[..]);
}

/// Tests that when the `SslStream` is created as a server stream, the protocols
/// are correctly advertised to the client.
#[test]
fn test_alpn_server_advertise_multiple() {
    let mut server = Server::builder();
    server.ctx().set_alpn_select_callback(|_, client| {
        ssl::select_next_proto(b"\x08http/1.1\x08spdy/3.1", client).ok_or(ssl::AlpnError::NOACK)
    });
    let server = server.build();

    let mut client = server.client();
    client.ctx().set_alpn_protos(b"\x08spdy/3.1").unwrap();
    let s = client.connect();
    assert_eq!(s.ssl().selected_alpn_protocol(), Some(&b"spdy/3.1"[..]));
}

#[test]
#[cfg(any(ossl110, boringssl, awslc))]
fn test_alpn_server_select_none_fatal() {
    let mut server = Server::builder();
    server.ctx().set_alpn_select_callback(|_, client| {
        ssl::select_next_proto(b"\x08http/1.1\x08spdy/3.1", client)
            .ok_or(ssl::AlpnError::ALERT_FATAL)
    });
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    client.ctx().set_alpn_protos(b"\x06http/2").unwrap();
    client.connect_err();
}

#[test]
fn test_alpn_server_select_none() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server.ctx().set_alpn_select_callback(|_, client| {
        CALLED_BACK.store(true, Ordering::SeqCst);
        ssl::select_next_proto(b"\x08http/1.1\x08spdy/3.1", client).ok_or(ssl::AlpnError::NOACK)
    });
    let server = server.build();

    let mut client = server.client();
    client.ctx().set_alpn_protos(b"\x06http/2").unwrap();
    let s = client.connect();
    assert_eq!(None, s.ssl().selected_alpn_protocol());
    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn test_alpn_server_unilateral() {
    let server = Server::builder().build();

    let mut client = server.client();
    client.ctx().set_alpn_protos(b"\x06http/2").unwrap();
    let s = client.connect();
    assert_eq!(None, s.ssl().selected_alpn_protocol());
}

#[test]
#[should_panic(expected = "blammo")]
fn write_panic() {
    struct ExplodingStream(TcpStream);

    impl Read for ExplodingStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for ExplodingStream {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("blammo");
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let stream = ExplodingStream(server.connect_tcp());

    let ctx = SslContext::builder(SslMethod::tls()).unwrap();
    let _ = Ssl::new(&ctx.build()).unwrap().connect(stream);
}

#[test]
#[should_panic(expected = "blammo")]
fn read_panic() {
    struct ExplodingStream(TcpStream);

    impl Read for ExplodingStream {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("blammo");
        }
    }

    impl Write for ExplodingStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let stream = ExplodingStream(server.connect_tcp());

    let ctx = SslContext::builder(SslMethod::tls()).unwrap();
    let _ = Ssl::new(&ctx.build()).unwrap().connect(stream);
}

#[test]
#[should_panic(expected = "blammo")]
fn flush_panic() {
    struct ExplodingStream(TcpStream);

    impl Read for ExplodingStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for ExplodingStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            panic!("blammo");
        }
    }

    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let stream = ExplodingStream(server.connect_tcp());

    let ctx = SslContext::builder(SslMethod::tls()).unwrap();
    let _ = Ssl::new(&ctx.build()).unwrap().connect(stream);
}

#[test]
fn refcount_ssl_context() {
    let mut ssl = {
        let ctx = SslContext::builder(SslMethod::tls()).unwrap();
        ssl::Ssl::new(&ctx.build()).unwrap()
    };

    {
        let new_ctx_a = SslContext::builder(SslMethod::tls()).unwrap().build();
        ssl.set_ssl_context(&new_ctx_a).unwrap();
    }
}

#[test]
#[cfg_attr(libressl, ignore)]
#[cfg_attr(target_os = "windows", ignore)]
#[cfg_attr(all(target_os = "macos", feature = "vendored"), ignore)]
fn default_verify_paths() {
    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    ctx.set_default_verify_paths().unwrap();
    ctx.set_verify(SslVerifyMode::PEER);
    #[cfg(ossl400)]
    ctx.set_options(super::SslOptions::IGNORE_UNEXPECTED_EOF);
    let ctx = ctx.build();
    let s = match TcpStream::connect("google.com:443") {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut ssl = Ssl::new(&ctx).unwrap();
    ssl.set_hostname("google.com").unwrap();
    let mut socket = ssl.connect(s).unwrap();

    socket.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    let mut result = vec![];
    socket.read_to_end(&mut result).unwrap();

    println!("{}", String::from_utf8_lossy(&result));
    assert!(result.starts_with(b"HTTP/1.0"));
    assert!(result.ends_with(b"</HTML>\r\n") || result.ends_with(b"</html>"));
}

#[test]
fn verify_mode_round_trip() {
    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    let mut mode = SslVerifyMode::PEER;
    mode |= SslVerifyMode::FAIL_IF_NO_PEER_CERT;
    #[cfg(not(any(boringssl, awslc)))]
    {
        mode |= SslVerifyMode::CLIENT_ONCE;
    }
    #[cfg(ossl111)]
    {
        mode |= SslVerifyMode::POST_HANDSHAKE;
    }
    ctx.set_verify(mode);

    let ctx = ctx.build();
    assert_eq!(ctx.verify_mode(), mode);
    let ssl = Ssl::new(&ctx).unwrap();
    assert_eq!(ssl.verify_mode(), mode);
}

#[test]
fn add_extra_chain_cert() {
    let cert = X509::from_pem(CERT).unwrap();
    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    ctx.add_extra_chain_cert(cert).unwrap();
}

#[test]
#[cfg(ossl110)]
fn verify_valid_hostname() {
    let server = Server::builder().build();

    let mut client = server.client();
    client.ctx().set_ca_file("test/root-ca.pem").unwrap();
    client.ctx().set_verify(SslVerifyMode::PEER);

    let mut client = client.build().builder();
    client
        .ssl()
        .param_mut()
        .set_hostflags(X509CheckFlags::NO_PARTIAL_WILDCARDS);
    client.ssl().param_mut().set_host("foobar.com").unwrap();
    client.connect();
}

#[test]
#[cfg(ossl110)]
fn verify_invalid_hostname() {
    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    client.ctx().set_ca_file("test/root-ca.pem").unwrap();
    client.ctx().set_verify(SslVerifyMode::PEER);

    let mut client = client.build().builder();
    client
        .ssl()
        .param_mut()
        .set_hostflags(X509CheckFlags::NO_PARTIAL_WILDCARDS);
    client.ssl().param_mut().set_host("bogus.com").unwrap();
    client.connect_err();
}

#[test]
fn connector_valid_hostname() {
    let server = Server::builder().build();

    let mut connector = SslConnector::builder(SslMethod::tls()).unwrap();
    connector.set_ca_file("test/root-ca.pem").unwrap();

    let s = server.connect_tcp();
    let mut s = connector.build().connect("foobar.com", s).unwrap();
    s.read_exact(&mut [0]).unwrap();
}

#[test]
fn connector_invalid_hostname() {
    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let mut connector = SslConnector::builder(SslMethod::tls()).unwrap();
    connector.set_ca_file("test/root-ca.pem").unwrap();

    let s = server.connect_tcp();
    connector.build().connect("bogus.com", s).unwrap_err();
}

#[test]
fn connector_invalid_no_hostname_verification() {
    let server = Server::builder().build();

    let mut connector = SslConnector::builder(SslMethod::tls()).unwrap();
    connector.set_ca_file("test/root-ca.pem").unwrap();

    let s = server.connect_tcp();
    let mut s = connector
        .build()
        .configure()
        .unwrap()
        .verify_hostname(false)
        .connect("bogus.com", s)
        .unwrap();
    s.read_exact(&mut [0]).unwrap();
}

#[test]
fn connector_no_hostname_still_verifies() {
    let mut server = Server::builder();
    server.should_error();
    let server = server.build();

    let connector = SslConnector::builder(SslMethod::tls()).unwrap().build();

    let s = server.connect_tcp();
    assert!(connector
        .configure()
        .unwrap()
        .verify_hostname(false)
        .connect("fizzbuzz.com", s)
        .is_err());
}

#[test]
fn connector_can_disable_verify() {
    let server = Server::builder().build();

    let mut connector = SslConnector::builder(SslMethod::tls()).unwrap();
    connector.set_verify(SslVerifyMode::NONE);
    let connector = connector.build();

    let s = server.connect_tcp();
    let mut s = connector
        .configure()
        .unwrap()
        .connect("fizzbuzz.com", s)
        .unwrap();
    s.read_exact(&mut [0]).unwrap();
}

#[test]
fn connector_does_use_sni_with_dnsnames() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut builder = Server::builder();
    builder.ctx().set_servername_callback(|ssl, _| {
        assert_eq!(ssl.servername(NameType::HOST_NAME), Some("foobar.com"));
        CALLED_BACK.store(true, Ordering::SeqCst);
        Ok(())
    });
    let server = builder.build();

    let mut connector = SslConnector::builder(SslMethod::tls()).unwrap();
    connector.set_ca_file("test/root-ca.pem").unwrap();

    let s = server.connect_tcp();
    let mut s = connector
        .build()
        .configure()
        .unwrap()
        .connect("foobar.com", s)
        .unwrap();
    s.read_exact(&mut [0]).unwrap();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn connector_doesnt_use_sni_with_ips() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut builder = Server::builder();
    builder.ctx().set_servername_callback(|ssl, _| {
        assert_eq!(ssl.servername(NameType::HOST_NAME), None);
        CALLED_BACK.store(true, Ordering::SeqCst);
        Ok(())
    });
    let server = builder.build();

    let mut connector = SslConnector::builder(SslMethod::tls()).unwrap();
    // The server's cert isn't issued for 127.0.0.1 but we don't care for this test.
    connector.set_verify(SslVerifyMode::NONE);

    let s = server.connect_tcp();
    let mut s = connector
        .build()
        .configure()
        .unwrap()
        .connect("127.0.0.1", s)
        .unwrap();
    s.read_exact(&mut [0]).unwrap();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

fn test_mozilla_server(new: fn(SslMethod) -> Result<SslAcceptorBuilder, ErrorStack>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let t = thread::spawn(move || {
        let key = PKey::private_key_from_pem(KEY).unwrap();
        let cert = X509::from_pem(CERT).unwrap();
        let mut acceptor = new(SslMethod::tls()).unwrap();
        acceptor.set_private_key(&key).unwrap();
        acceptor.set_certificate(&cert).unwrap();
        let acceptor = acceptor.build();
        let stream = listener.accept().unwrap().0;
        let mut stream = acceptor.accept(stream).unwrap();

        stream.write_all(b"hello").unwrap();
    });

    let mut connector = SslConnector::builder(SslMethod::tls()).unwrap();
    connector.set_ca_file("test/root-ca.pem").unwrap();
    let connector = connector.build();

    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut stream = connector.connect("foobar.com", stream).unwrap();

    let mut buf = [0; 5];
    stream.read_exact(&mut buf).unwrap();
    assert_eq!(b"hello", &buf);

    t.join().unwrap();
}

#[test]
fn connector_client_server_mozilla_intermediate() {
    test_mozilla_server(SslAcceptor::mozilla_intermediate);
}

#[test]
fn connector_client_server_mozilla_modern() {
    test_mozilla_server(SslAcceptor::mozilla_modern);
}

#[test]
fn connector_client_server_mozilla_intermediate_v5() {
    test_mozilla_server(SslAcceptor::mozilla_intermediate_v5);
}

#[test]
#[cfg(any(ossl111, libressl))]
fn connector_client_server_mozilla_modern_v5() {
    test_mozilla_server(SslAcceptor::mozilla_modern_v5);
}

#[test]
fn shutdown() {
    let mut server = Server::builder();
    server.io_cb(|mut s| {
        assert_eq!(s.read(&mut [0]).unwrap(), 0);
        assert_eq!(s.shutdown().unwrap(), ShutdownResult::Received);
    });
    let server = server.build();

    let mut s = server.client().connect();

    assert_eq!(s.get_shutdown(), ShutdownState::empty());
    assert_eq!(s.shutdown().unwrap(), ShutdownResult::Sent);
    assert_eq!(s.get_shutdown(), ShutdownState::SENT);
    assert_eq!(s.shutdown().unwrap(), ShutdownResult::Received);
    assert_eq!(
        s.get_shutdown(),
        ShutdownState::SENT | ShutdownState::RECEIVED
    );
}

#[test]
fn client_ca_list() {
    let names = X509Name::load_client_ca_file("test/root-ca.pem").unwrap();
    assert_eq!(names.len(), 1);

    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    ctx.set_client_ca_list(names);
}

#[test]
fn cert_store() {
    let server = Server::builder().build();

    let mut client = server.client();
    let cert = X509::from_pem(ROOT_CERT).unwrap();
    client.ctx().cert_store_mut().add_cert(cert).unwrap();
    client.ctx().set_verify(SslVerifyMode::PEER);

    client.connect();
}

#[test]
#[cfg_attr(any(boringssl, awslc, ossl400), ignore)]
fn tmp_dh_callback() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server.ctx().set_tmp_dh_callback(|_, _, _| {
        CALLED_BACK.store(true, Ordering::SeqCst);
        let dh = include_bytes!("../../../test/dhparams.pem");
        Dh::params_from_pem(dh)
    });

    let server = server.build();

    let mut client = server.client();
    // TLS 1.3 has no DH suites, so make sure we don't pick that version
    #[cfg(any(ossl111, libressl))]
    client.ctx().set_options(super::SslOptions::NO_TLSV1_3);
    client.ctx().set_cipher_list("EDH").unwrap();
    client.connect();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
#[cfg_attr(any(boringssl, awslc, ossl400), ignore)]
fn tmp_dh_callback_ssl() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server.ssl_cb(|ssl| {
        ssl.set_tmp_dh_callback(|_, _, _| {
            CALLED_BACK.store(true, Ordering::SeqCst);
            let dh = include_bytes!("../../../test/dhparams.pem");
            Dh::params_from_pem(dh)
        });
    });

    let server = server.build();

    let mut client = server.client();
    // TLS 1.3 has no DH suites, so make sure we don't pick that version
    #[cfg(any(ossl111, libressl))]
    client.ctx().set_options(super::SslOptions::NO_TLSV1_3);
    client.ctx().set_cipher_list("EDH").unwrap();
    client.connect();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn idle_session() {
    let ctx = SslContext::builder(SslMethod::tls()).unwrap().build();
    let ssl = Ssl::new(&ctx).unwrap();
    assert!(ssl.session().is_none());
}

/// LibreSSL 3.2.1 enabled TLSv1.3 by default for clients and sessions do
/// not work due to lack of PSK support. The test passes with NO_TLSV1_3,
/// but let's ignore it until LibreSSL supports it out of the box.
#[test]
#[cfg_attr(libressl, ignore)]
fn active_session() {
    let server = Server::builder().build();

    let s = server.client().connect();

    let session = s.ssl().session().unwrap();
    let len = session.master_key_len();
    let mut buf = vec![0; len - 1];
    let copied = session.master_key(&mut buf);
    assert_eq!(copied, buf.len());
    let mut buf = vec![0; len + 1];
    let copied = session.master_key(&mut buf);
    assert_eq!(copied, len);
}

#[test]
#[cfg(not(any(boringssl, awslc)))]
fn status_callbacks() {
    static CALLED_BACK_SERVER: AtomicBool = AtomicBool::new(false);
    static CALLED_BACK_CLIENT: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server
        .ctx()
        .set_status_callback(|ssl| {
            CALLED_BACK_SERVER.store(true, Ordering::SeqCst);
            let response = OcspResponse::create(OcspResponseStatus::UNAUTHORIZED, None).unwrap();
            let response = response.to_der().unwrap();
            ssl.set_ocsp_status(&response).unwrap();
            Ok(true)
        })
        .unwrap();

    let server = server.build();

    let mut client = server.client();
    client
        .ctx()
        .set_status_callback(|ssl| {
            CALLED_BACK_CLIENT.store(true, Ordering::SeqCst);
            let response = OcspResponse::from_der(ssl.ocsp_status().unwrap()).unwrap();
            assert_eq!(response.status(), OcspResponseStatus::UNAUTHORIZED);
            Ok(true)
        })
        .unwrap();

    let mut client = client.build().builder();
    client.ssl().set_status_type(StatusType::OCSP).unwrap();

    client.connect();

    assert!(CALLED_BACK_SERVER.load(Ordering::SeqCst));
    assert!(CALLED_BACK_CLIENT.load(Ordering::SeqCst));
}

/// LibreSSL 3.2.1 enabled TLSv1.3 by default for clients and sessions do
/// not work due to lack of PSK support. The test passes with NO_TLSV1_3,
/// but let's ignore it until LibreSSL supports it out of the box.
#[test]
#[cfg_attr(libressl, ignore)]
fn new_session_callback() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server.ctx().set_session_id_context(b"foo").unwrap();

    let server = server.build();

    let mut client = server.client();

    client
        .ctx()
        .set_session_cache_mode(SslSessionCacheMode::CLIENT | SslSessionCacheMode::NO_INTERNAL);
    client
        .ctx()
        .set_new_session_callback(|_, _| CALLED_BACK.store(true, Ordering::SeqCst));

    client.connect();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

/// LibreSSL 3.2.1 enabled TLSv1.3 by default for clients and sessions do
/// not work due to lack of PSK support. The test passes with NO_TLSV1_3,
/// but let's ignore it until LibreSSL supports it out of the box.
#[test]
#[cfg_attr(libressl, ignore)]
fn new_session_callback_swapped_ctx() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server.ctx().set_session_id_context(b"foo").unwrap();

    let server = server.build();

    let mut client = server.client();

    client
        .ctx()
        .set_session_cache_mode(SslSessionCacheMode::CLIENT | SslSessionCacheMode::NO_INTERNAL);
    client
        .ctx()
        .set_new_session_callback(|_, _| CALLED_BACK.store(true, Ordering::SeqCst));

    let mut client = client.build().builder();

    let ctx = SslContextBuilder::new(SslMethod::tls()).unwrap().build();
    client.ssl().set_ssl_context(&ctx).unwrap();

    client.connect();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
fn keying_export() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let label = "EXPERIMENTAL test";
    let context = b"my context";

    let guard = thread::spawn(move || {
        let stream = listener.accept().unwrap().0;
        let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
        ctx.set_certificate_file(Path::new("test/cert.pem"), SslFiletype::PEM)
            .unwrap();
        ctx.set_private_key_file(Path::new("test/key.pem"), SslFiletype::PEM)
            .unwrap();
        let ssl = Ssl::new(&ctx.build()).unwrap();
        let mut stream = ssl.accept(stream).unwrap();

        let mut buf = [0; 32];
        stream
            .ssl()
            .export_keying_material(&mut buf, label, Some(context))
            .unwrap();

        stream.write_all(&[0]).unwrap();

        buf
    });

    let stream = TcpStream::connect(addr).unwrap();
    let ctx = SslContext::builder(SslMethod::tls()).unwrap();
    let ssl = Ssl::new(&ctx.build()).unwrap();
    let mut stream = ssl.connect(stream).unwrap();

    let mut buf = [1; 32];
    stream
        .ssl()
        .export_keying_material(&mut buf, label, Some(context))
        .unwrap();

    stream.read_exact(&mut [0]).unwrap();

    let buf2 = guard.join().unwrap();

    assert_eq!(buf, buf2);
}

#[test]
#[cfg(any(ossl110, libressl))]
fn no_version_overlap() {
    let mut server = Server::builder();
    server.ctx().set_min_proto_version(None).unwrap();
    server
        .ctx()
        .set_max_proto_version(Some(SslVersion::TLS1_1))
        .unwrap();
    #[cfg(any(ossl110g, libressl))]
    assert_eq!(server.ctx().max_proto_version(), Some(SslVersion::TLS1_1));
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    client
        .ctx()
        .set_min_proto_version(Some(SslVersion::TLS1_2))
        .unwrap();
    #[cfg(ossl110g)]
    assert_eq!(client.ctx().min_proto_version(), Some(SslVersion::TLS1_2));
    client.ctx().set_max_proto_version(None).unwrap();

    client.connect_err();
}

#[test]
#[cfg(ossl111)]
fn custom_extensions() {
    static FOUND_EXTENSION: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server
        .ctx()
        .add_custom_ext(
            12345,
            ExtensionContext::CLIENT_HELLO,
            |_, _, _| -> Result<Option<&'static [u8]>, _> { unreachable!() },
            |_, _, data, _| {
                FOUND_EXTENSION.store(data == b"hello", Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();

    let server = server.build();

    let mut client = server.client();
    client
        .ctx()
        .add_custom_ext(
            12345,
            ssl::ExtensionContext::CLIENT_HELLO,
            |_, _, _| Ok(Some(b"hello")),
            |_, _, _, _| unreachable!(),
        )
        .unwrap();

    client.connect();

    assert!(FOUND_EXTENSION.load(Ordering::SeqCst));
}

#[test]
#[cfg(ossl111)]
fn custom_extensions_inline_buffer() {
    static FOUND_EXTENSION: AtomicBool = AtomicBool::new(false);
    const EXPECTED: [u8; 128] = [0xAB; 128];

    let mut server = Server::builder();
    server
        .ctx()
        .add_custom_ext(
            12345,
            ExtensionContext::CLIENT_HELLO,
            |_, _, _| -> Result<Option<[u8; 128]>, _> { unreachable!() },
            |_, _, data, _| {
                FOUND_EXTENSION.store(data == EXPECTED, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();

    let server = server.build();

    let mut client = server.client();
    client
        .ctx()
        .add_custom_ext(
            12345,
            ssl::ExtensionContext::CLIENT_HELLO,
            move |_, _, _| Ok(Some(EXPECTED)),
            |_, _, _, _| unreachable!(),
        )
        .unwrap();

    client.connect();

    assert!(FOUND_EXTENSION.load(Ordering::SeqCst));
}

fn _check_kinds() {
    fn is_send<T: Send>() {}
    fn is_sync<T: Sync>() {}

    is_send::<SslStream<TcpStream>>();
    is_sync::<SslStream<TcpStream>>();
}

#[cfg(ossl111)]
#[derive(Debug)]
struct MemoryStream {
    incoming: io::Cursor<Vec<u8>>,
    outgoing: Vec<u8>,
}

#[cfg(ossl111)]
impl MemoryStream {
    fn new() -> Self {
        Self {
            incoming: io::Cursor::new(Vec::new()),
            outgoing: Vec::new(),
        }
    }

    fn extend_incoming(&mut self, data: &[u8]) {
        self.incoming.get_mut().extend_from_slice(data);
    }

    fn take_outgoing(&mut self) -> Vec<u8> {
        mem::take(&mut self.outgoing)
    }
}

#[cfg(ossl111)]
impl Read for MemoryStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.incoming.read(buf)?;
        if self.incoming.position() == self.incoming.get_ref().len() as u64 {
            self.incoming.set_position(0);
            self.incoming.get_mut().clear();
        }
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "no data available",
            ));
        }
        Ok(n)
    }
}

#[cfg(ossl111)]
impl Write for MemoryStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.outgoing.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(ossl111)]
fn send(from: &mut MemoryStream, to: &mut MemoryStream) {
    to.extend_incoming(&from.take_outgoing());
}

#[test]
#[cfg(ossl111)]
fn stateless() {
    //
    // Setup
    //

    let mut client_ctx = SslContext::builder(SslMethod::tls()).unwrap();
    client_ctx.clear_options(SslOptions::ENABLE_MIDDLEBOX_COMPAT);
    let mut client_stream =
        SslStream::new(Ssl::new(&client_ctx.build()).unwrap(), MemoryStream::new()).unwrap();

    let mut server_ctx = SslContext::builder(SslMethod::tls()).unwrap();
    server_ctx
        .set_certificate_file(Path::new("test/cert.pem"), SslFiletype::PEM)
        .unwrap();
    server_ctx
        .set_private_key_file(Path::new("test/key.pem"), SslFiletype::PEM)
        .unwrap();
    const COOKIE: &[u8] = b"chocolate chip";
    server_ctx.set_stateless_cookie_generate_cb(|_tls, buf| {
        buf[0..COOKIE.len()].copy_from_slice(COOKIE);
        Ok(COOKIE.len())
    });
    server_ctx.set_stateless_cookie_verify_cb(|_tls, buf| buf == COOKIE);
    let mut server_stream =
        SslStream::new(Ssl::new(&server_ctx.build()).unwrap(), MemoryStream::new()).unwrap();

    //
    // Handshake
    //

    // Initial ClientHello
    client_stream.connect().unwrap_err();
    send(client_stream.get_mut(), server_stream.get_mut());
    // HelloRetryRequest
    assert!(!server_stream.stateless().unwrap());
    send(server_stream.get_mut(), client_stream.get_mut());
    // Second ClientHello
    client_stream.do_handshake().unwrap_err();
    send(client_stream.get_mut(), server_stream.get_mut());
    // OldServerHello
    assert!(server_stream.stateless().unwrap());
    server_stream.accept().unwrap_err();
    send(server_stream.get_mut(), client_stream.get_mut());
    // Finished
    client_stream.do_handshake().unwrap();
    send(client_stream.get_mut(), server_stream.get_mut());
    server_stream.do_handshake().unwrap();
}

#[cfg(not(osslconf = "OPENSSL_NO_PSK"))]
#[test]
fn psk_ciphers() {
    const CIPHER: &str = "PSK-AES256-CBC-SHA";
    const PSK: &[u8] = b"thisisaverysecurekey";
    const CLIENT_IDENT: &[u8] = b"thisisaclient";
    static CLIENT_CALLED: AtomicBool = AtomicBool::new(false);
    static SERVER_CALLED: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server.ctx().set_cipher_list(CIPHER).unwrap();
    server.ctx().set_psk_server_callback(|_, identity, psk| {
        assert!(identity.unwrap_or(&[]) == CLIENT_IDENT);
        psk[..PSK.len()].copy_from_slice(PSK);
        SERVER_CALLED.store(true, Ordering::SeqCst);
        Ok(PSK.len())
    });

    let server = server.build();

    let mut client = server.client();
    // This test relies on TLS 1.2 suites
    #[cfg(any(boringssl, ossl111, awslc))]
    client.ctx().set_options(super::SslOptions::NO_TLSV1_3);
    client.ctx().set_cipher_list(CIPHER).unwrap();
    client
        .ctx()
        .set_psk_client_callback(move |_, _, identity, psk| {
            identity[..CLIENT_IDENT.len()].copy_from_slice(CLIENT_IDENT);
            identity[CLIENT_IDENT.len()] = 0;
            psk[..PSK.len()].copy_from_slice(PSK);
            CLIENT_CALLED.store(true, Ordering::SeqCst);
            Ok(PSK.len())
        });

    client.connect();

    assert!(SERVER_CALLED.load(Ordering::SeqCst));
    assert!(CLIENT_CALLED.load(Ordering::SeqCst));
}

// Regression tests: the PSK/cookie trampolines used to forward the callback's
// returned `usize` to OpenSSL without checking it against the slice length.

#[cfg(not(osslconf = "OPENSSL_NO_PSK"))]
#[cfg(target_pointer_width = "64")]
#[test]
fn psk_client_cb_oversize_psk_len_rejected() {
    // Without the fix, `psk_len as u32` truncates the returned length; the low
    // 32 bits match `PSK.len()` and slip past OpenSSL's `> PSK_MAX_PSK_LEN`
    // check. (Rust's slice length equals `PSK_MAX_PSK_LEN`, so truncation is
    // the only way to differentiate — hence the 64-bit guard.)
    const CIPHER: &str = "PSK-AES256-CBC-SHA";
    const PSK: &[u8] = b"thisisaverysecurekey";
    const CLIENT_IDENT: &[u8] = b"thisisaclient";

    let mut server = Server::builder();
    server.ctx().set_cipher_list(CIPHER).unwrap();
    server.ctx().set_psk_server_callback(|_, _identity, psk| {
        psk[..PSK.len()].copy_from_slice(PSK);
        Ok(PSK.len())
    });
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    #[cfg(any(boringssl, ossl111, awslc))]
    client.ctx().set_options(SslOptions::NO_TLSV1_3);
    client.ctx().set_cipher_list(CIPHER).unwrap();
    client
        .ctx()
        .set_psk_client_callback(move |_, _, identity, psk| {
            identity[..CLIENT_IDENT.len()].copy_from_slice(CLIENT_IDENT);
            identity[CLIENT_IDENT.len()] = 0;
            psk[..PSK.len()].copy_from_slice(PSK);
            Ok((u32::MAX as usize) + 1 + PSK.len())
        });

    client.connect_err();
}

#[cfg(not(osslconf = "OPENSSL_NO_PSK"))]
#[cfg(target_pointer_width = "64")]
#[test]
fn psk_server_cb_oversize_psk_len_rejected() {
    // Server-side counterpart — same `as u32` truncation bypass.
    const CIPHER: &str = "PSK-AES256-CBC-SHA";
    const PSK: &[u8] = b"thisisaverysecurekey";
    const CLIENT_IDENT: &[u8] = b"thisisaclient";

    let mut server = Server::builder();
    server.ctx().set_cipher_list(CIPHER).unwrap();
    server.ctx().set_psk_server_callback(|_, _identity, psk| {
        psk[..PSK.len()].copy_from_slice(PSK);
        Ok((u32::MAX as usize) + 1 + PSK.len())
    });
    server.should_error();
    let server = server.build();

    let mut client = server.client();
    #[cfg(any(boringssl, ossl111, awslc))]
    client.ctx().set_options(SslOptions::NO_TLSV1_3);
    client.ctx().set_cipher_list(CIPHER).unwrap();
    client
        .ctx()
        .set_psk_client_callback(move |_, _, identity, psk| {
            identity[..CLIENT_IDENT.len()].copy_from_slice(CLIENT_IDENT);
            identity[CLIENT_IDENT.len()] = 0;
            psk[..PSK.len()].copy_from_slice(PSK);
            Ok(PSK.len())
        });

    client.connect_err();
}

#[test]
#[cfg(ossl111)]
fn stateless_cookie_cb_oversize_length_rejected() {
    // Callback claims a length past the slice end. The fix makes the
    // trampoline report failure so stateless() errors cleanly.
    let mut client_ctx = SslContext::builder(SslMethod::tls()).unwrap();
    client_ctx.clear_options(SslOptions::ENABLE_MIDDLEBOX_COMPAT);
    let mut client_stream =
        SslStream::new(Ssl::new(&client_ctx.build()).unwrap(), MemoryStream::new()).unwrap();

    let mut server_ctx = SslContext::builder(SslMethod::tls()).unwrap();
    server_ctx
        .set_certificate_file(Path::new("test/cert.pem"), SslFiletype::PEM)
        .unwrap();
    server_ctx
        .set_private_key_file(Path::new("test/key.pem"), SslFiletype::PEM)
        .unwrap();
    server_ctx.set_stateless_cookie_generate_cb(|_, buf| Ok(buf.len() + 1));
    server_ctx.set_stateless_cookie_verify_cb(|_, _| true);
    let mut server_stream =
        SslStream::new(Ssl::new(&server_ctx.build()).unwrap(), MemoryStream::new()).unwrap();

    client_stream.connect().unwrap_err();
    send(client_stream.get_mut(), server_stream.get_mut());
    assert!(server_stream.stateless().is_err());
}

#[test]
#[cfg(not(any(boringssl, awslc)))]
fn dtls_cookie_generate_cb_oversize_length_rejected() {
    // Rust hands the callback `DTLS1_COOKIE_LENGTH - 1` bytes but OpenSSL's
    // internal cookie buffer is `DTLS1_COOKIE_LENGTH`; returning `buf.len() + 1`
    // passes OpenSSL's `cookie_leni > sizeof(s->d1->cookie)` check. Without the
    // fix, the server sends a HelloVerifyRequest containing one unwritten byte
    // and the verify callback fires on the client's echo.
    static VERIFY_CALLED: AtomicBool = AtomicBool::new(false);
    VERIFY_CALLED.store(false, Ordering::SeqCst);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let stream = listener.accept().unwrap().0;
        let mut ctx = SslContext::builder(SslMethod::dtls()).unwrap();
        ctx.set_certificate_file(Path::new("test/cert.pem"), SslFiletype::PEM)
            .unwrap();
        ctx.set_private_key_file(Path::new("test/key.pem"), SslFiletype::PEM)
            .unwrap();
        ctx.set_options(SslOptions::COOKIE_EXCHANGE);
        ctx.set_cookie_generate_cb(|_, buf| Ok(buf.len() + 1));
        ctx.set_cookie_verify_cb(|_, _| {
            VERIFY_CALLED.store(true, Ordering::SeqCst);
            true
        });
        let mut ssl = Ssl::new(&ctx.build()).unwrap();
        ssl.set_mtu(1500).unwrap();
        let _ = ssl.accept(stream);
    });

    let stream = TcpStream::connect(addr).unwrap();
    let ctx = SslContext::builder(SslMethod::dtls()).unwrap();
    let mut ssl = Ssl::new(&ctx.build()).unwrap();
    ssl.set_mtu(1500).unwrap();
    let _ = ssl.connect(stream);

    server.join().unwrap();
    assert!(!VERIFY_CALLED.load(Ordering::SeqCst));
}

#[test]
fn sni_callback_swapped_ctx() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();

    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    ctx.set_servername_callback(|_, _| {
        CALLED_BACK.store(true, Ordering::SeqCst);
        Ok(())
    });

    let keyed_ctx = mem::replace(server.ctx(), ctx).build();
    server.ssl_cb(move |ssl| ssl.set_ssl_context(&keyed_ctx).unwrap());

    let server = server.build();

    server.client().connect();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

// Regression test: the verify_callback function pointer is per-SSL (copied from the SSL_CTX into
// the SSL at SSL_new time) and is *not* updated when SSL_set_SSL_CTX swaps the context. Before the
// fix, the trampoline still invoked the original raw_verify::<F_a> after a swap, but it looked up
// the closure on the *current* (swapped) ctx, which doesn't have an F_a entry — the .expect() then
// aborted the process via a panic across an extern "C" boundary.
#[test]
fn verify_callback_after_swapped_ctx() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let server = Server::builder().build();

    let mut client = server.client();
    client
        .ctx()
        .set_verify_callback(SslVerifyMode::PEER, |_, _| {
            CALLED_BACK.store(true, Ordering::SeqCst);
            true
        });

    let mut client = client.build().builder();

    // Swap to a fresh ctx that has no verify callback registered. The per-SSL verify function
    // pointer raw_verify::<F> is unaffected by the swap and will still fire during the handshake;
    // it must still find the original closure and not abort.
    let other_ctx = SslContextBuilder::new(SslMethod::tls()).unwrap().build();
    client.ssl().set_ssl_context(&other_ctx).unwrap();

    client.connect();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
#[cfg(ossl111)]
fn client_hello() {
    static CALLED_BACK: AtomicBool = AtomicBool::new(false);

    let mut server = Server::builder();
    server.ctx().set_client_hello_callback(|ssl, _| {
        assert!(!ssl.client_hello_isv2());
        assert_eq!(ssl.client_hello_legacy_version(), Some(SslVersion::TLS1_2));
        assert!(ssl.client_hello_random().is_some());
        assert!(ssl.client_hello_session_id().is_some());
        assert!(ssl.client_hello_ciphers().is_some());
        assert!(ssl.client_hello_compression_methods().is_some());
        assert!(ssl
            .bytes_to_cipher_list(ssl.client_hello_ciphers().unwrap(), ssl.client_hello_isv2())
            .is_ok());

        CALLED_BACK.store(true, Ordering::SeqCst);
        Ok(ClientHelloResponse::SUCCESS)
    });

    let server = server.build();
    server.client().connect();

    assert!(CALLED_BACK.load(Ordering::SeqCst));
}

#[test]
#[cfg(ossl111)]
fn openssl_cipher_name() {
    assert_eq!(
        super::cipher_name("TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA384"),
        "ECDHE-RSA-AES256-SHA384",
    );

    assert_eq!(super::cipher_name("asdf"), "(NONE)");
}

#[test]
fn session_cache_size() {
    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    ctx.set_session_cache_size(1234);
    let ctx = ctx.build();
    assert_eq!(ctx.session_cache_size(), 1234);
}

#[test]
#[cfg(ossl110)]
fn add_chain_cert() {
    let ctx = SslContext::builder(SslMethod::tls()).unwrap().build();
    let cert = X509::from_pem(CERT).unwrap();
    let mut ssl = Ssl::new(&ctx).unwrap();
    assert!(ssl.add_chain_cert(cert).is_ok());
}
#[test]
#[cfg(ossl111)]
fn set_ssl_certificate_key_related_api() {
    let cert_str: &str = include_str!("../../../test/cert.pem");
    let key_str: &str = include_str!("../../../test/key.pem");
    let ctx = SslContext::builder(SslMethod::tls()).unwrap().build();
    let cert_x509 = X509::from_pem(CERT).unwrap();
    let mut ssl = Ssl::new(&ctx).unwrap();
    assert!(ssl.set_method(SslMethod::tls()).is_ok());
    ssl.set_private_key_file("test/key.pem", SslFiletype::PEM)
        .unwrap();
    {
        let pkey = String::from_utf8(
            ssl.private_key()
                .unwrap()
                .private_key_to_pem_pkcs8()
                .unwrap(),
        )
        .unwrap();
        assert!(pkey.lines().eq(key_str.lines()));
    }
    let pkey = PKey::private_key_from_pem(KEY).unwrap();
    ssl.set_private_key(pkey.as_ref()).unwrap();
    {
        let pkey = String::from_utf8(
            ssl.private_key()
                .unwrap()
                .private_key_to_pem_pkcs8()
                .unwrap(),
        )
        .unwrap();
        assert!(pkey.lines().eq(key_str.lines()));
    }
    ssl.set_certificate(cert_x509.as_ref()).unwrap();
    let cert = String::from_utf8(ssl.certificate().unwrap().to_pem().unwrap()).unwrap();
    assert!(cert.lines().eq(cert_str.lines()));
    ssl.add_client_ca(cert_x509.as_ref()).unwrap();
    ssl.set_min_proto_version(Some(SslVersion::TLS1_2)).unwrap();
    ssl.set_max_proto_version(Some(SslVersion::TLS1_3)).unwrap();
    ssl.set_cipher_list("HIGH:!aNULL:!MD5").unwrap();
    ssl.set_ciphersuites("TLS_AES_128_GCM_SHA256").unwrap();
    let x509 = X509::from_pem(ROOT_CERT).unwrap();
    let mut builder = X509StoreBuilder::new().unwrap();
    builder.add_cert(x509).unwrap();
    let store = builder.build();
    ssl.set_verify_cert_store(store).unwrap();
}

#[test]
#[cfg(ossl110)]
fn test_ssl_set_cert_chain_file() {
    let ctx = SslContext::builder(SslMethod::tls()).unwrap().build();
    let mut ssl = Ssl::new(&ctx).unwrap();
    ssl.set_certificate_chain_file("test/cert.pem").unwrap();
}

#[test]
#[cfg(ossl111)]
fn set_num_tickets() {
    let mut ctx = SslContext::builder(SslMethod::tls_server()).unwrap();
    ctx.set_num_tickets(3).unwrap();
    let ctx = ctx.build();
    assert_eq!(3, ctx.num_tickets());

    let mut ssl = Ssl::new(&ctx).unwrap();
    ssl.set_num_tickets(5).unwrap();
    let ssl = ssl;
    assert_eq!(5, ssl.num_tickets());
}

#[test]
#[cfg(ossl110)]
fn set_security_level() {
    let mut ctx = SslContext::builder(SslMethod::tls_server()).unwrap();
    ctx.set_security_level(3);
    let ctx = ctx.build();
    assert_eq!(3, ctx.security_level());

    let mut ssl = Ssl::new(&ctx).unwrap();
    ssl.set_security_level(4);
    let ssl = ssl;
    assert_eq!(4, ssl.security_level());
}

#[test]
fn ssl_ctx_ex_data_leak() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);

    struct DropTest;

    impl Drop for DropTest {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    let idx = SslContext::new_ex_index().unwrap();

    let mut ctx = SslContext::builder(SslMethod::tls()).unwrap();
    ctx.set_ex_data(idx, DropTest);
    ctx.set_ex_data(idx, DropTest);
    assert_eq!(DROPS.load(Ordering::Relaxed), 1);

    drop(ctx);
    assert_eq!(DROPS.load(Ordering::Relaxed), 2);
}

#[test]
fn ssl_ex_data_leak() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);

    struct DropTest;

    impl Drop for DropTest {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    let idx = Ssl::new_ex_index().unwrap();

    let ctx = SslContext::builder(SslMethod::tls()).unwrap().build();
    let mut ssl = Ssl::new(&ctx).unwrap();
    ssl.set_ex_data(idx, DropTest);
    ssl.set_ex_data(idx, DropTest);
    assert_eq!(DROPS.load(Ordering::Relaxed), 1);

    drop(ssl);
    assert_eq!(DROPS.load(Ordering::Relaxed), 2);
}

#[test]
#[cfg(ossl111)]
fn cipher_id() {
    let mut server = Server::builder();
    server
        .ctx()
        .set_ciphersuites("TLS_AES_256_GCM_SHA384")
        .unwrap();
    let server = server.build();

    let client = server.client();
    let s = client.connect();
    let ssl = s.ssl();
    let cipher = ssl.current_cipher().unwrap();
    let cipher_id = cipher.protocol_id();
    assert_eq!(cipher_id, [0x13, 0x02]);
}
// AutoRouter B2 ownership tests. Synthetic loopback only; no trust-policy claim.
// Run this filtered module in a fresh process with the pinned vendored backend.
#[cfg(ossl300)]
mod autorouter_bound_sessions {
    use super::*;
    use crate::ssl::{BoundSessionError, ContextBoundSession, SslRef};
    use foreign_types::{ForeignType, ForeignTypeRef};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    type Captures = Arc<Mutex<Vec<Result<ContextBoundSession, BoundSessionError>>>>;

    fn initialize() {
        crate::init_without_config().unwrap();
        assert_eq!(
            crate::version::number(),
            0x30600030,
            "B2 requires pinned OpenSSL3.6.3"
        );
    }

    struct CallbackLifetime(Arc<AtomicUsize>);
    impl Drop for CallbackLifetime {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn client_context(
        version: SslVersion,
        captures: &Captures,
        dropped: Arc<AtomicUsize>,
    ) -> SslContext {
        initialize();
        let mut builder = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
        builder.set_min_proto_version(Some(version)).unwrap();
        builder.set_max_proto_version(Some(version)).unwrap();
        // Certificate qualification belongs to runtime matrix tests, not these
        // ownership tests using the upstream crate's public synthetic fixture.
        builder.set_verify(SslVerifyMode::NONE);
        builder
            .set_session_cache_mode(SslSessionCacheMode::CLIENT | SslSessionCacheMode::NO_INTERNAL);
        let weak = Arc::downgrade(captures);
        let lifetime = CallbackLifetime(dropped);
        builder.set_bound_new_session_callback(move |_, session| {
            let _keep_lifetime_in_callback = &lifetime;
            if let Some(captures) = weak.upgrade() {
                // Snapshot creation already finished; no OpenSSL operation
                // occurs while holding this application's cache mutex.
                captures.lock().unwrap().push(session);
            }
        });
        builder.build()
    }

    fn peer(
        version: SslVersion,
        attempts: usize,
    ) -> (
        SocketAddr,
        thread::JoinHandle<Result<Vec<bool>, &'static str>>,
    ) {
        initialize();
        let mut builder = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
        builder
            .set_certificate(&X509::from_pem(CERT).unwrap())
            .unwrap();
        builder
            .set_private_key(&PKey::private_key_from_pem(KEY).unwrap())
            .unwrap();
        builder.set_min_proto_version(Some(version)).unwrap();
        builder.set_max_proto_version(Some(version)).unwrap();
        builder
            .set_session_id_context(b"synthetic-bound-session")
            .unwrap();
        builder
            .set_session_cache_mode(SslSessionCacheMode::SERVER | SslSessionCacheMode::NO_INTERNAL);
        let context = builder.build();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let worker = thread::spawn(move || {
            let mut reused = Vec::new();
            for _ in 0..attempts {
                let deadline = Instant::now() + Duration::from_secs(3);
                let socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(_) => return Err("bounded synthetic accept failed"),
                    }
                };
                // macOS inherits the listener's nonblocking flag on accept;
                // this helper deliberately drives the blocking OpenSSL API.
                socket
                    .set_nonblocking(false)
                    .map_err(|_| "blocking fixture socket")?;
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .map_err(|_| "read deadline")?;
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .map_err(|_| "write deadline")?;
                let ssl = Ssl::new(&context).map_err(|_| "server allocation")?;
                let mut stream = ssl.accept(socket).map_err(|_| "server handshake")?;
                reused.push(stream.ssl().session_reused());
                stream
                    .write_all(&[1])
                    .map_err(|_| "server application write")?;
                let mut acknowledgement = [0];
                stream
                    .read_exact(&mut acknowledgement)
                    .map_err(|_| "server acknowledgement")?;
                if acknowledgement != [2] {
                    return Err("wrong synthetic acknowledgement");
                }
                // Send close_notify without requiring an abrupt client to reply.
                let _ = stream.shutdown();
            }
            Ok(reused)
        });
        (address, worker)
    }

    fn connect(mut ssl: Ssl, address: SocketAddr, clean: bool) -> bool {
        let socket = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        ssl.set_connect_state();
        let mut stream = ssl.connect(socket).unwrap();
        let reused = stream.ssl().session_reused();
        stream.read_exact(&mut [0]).unwrap(); // also processes TLS1.3 tickets
        stream.write_all(&[2]).unwrap();
        if clean {
            let _ = stream.shutdown();
            let _ = stream.shutdown();
        }
        // An unclean drop can mark the native session non-resumable. The saved
        // DER must remain independent of that mutable native object.
        drop(stream);
        reused
    }

    fn captured_session(captures: &Captures) -> ContextBoundSession {
        let captured = captures.lock().unwrap();
        assert!(!captured.is_empty(), "missing actual new-session callback");
        assert!(captured.iter().all(Result::is_ok));
        captured.last().unwrap().as_ref().unwrap().clone()
    }

    #[test]
    fn immutable_snapshots_resume_after_clean_and_unclean_close_tls12_tls13() {
        for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
            for clean in [false, true] {
                let captures = Arc::new(Mutex::new(Vec::new()));
                let context = client_context(version, &captures, Arc::new(AtomicUsize::new(0)));
                let (address, worker) = peer(version, 2);
                assert!(!connect(Ssl::new(&context).unwrap(), address, clean));
                let snapshot = captured_session(&captures);
                assert!(snapshot.encoded_len() > 0);
                let same_context = context.clone();
                let mut ssl = Ssl::new(&same_context).unwrap();
                ssl.set_bound_session(&snapshot.clone()).unwrap();
                assert!(connect(ssl, address, true));
                assert_eq!(worker.join().unwrap().unwrap(), [false, true]);
            }
        }
    }

    #[test]
    fn foreign_original_and_current_contexts_reject_before_changing_session() {
        let captures = Arc::new(Mutex::new(Vec::new()));
        let context = client_context(SslVersion::TLS1_2, &captures, Arc::new(AtomicUsize::new(0)));
        let (address, worker) = peer(SslVersion::TLS1_2, 1);
        connect(Ssl::new(&context).unwrap(), address, true);
        worker.join().unwrap().unwrap();
        let snapshot = captured_session(&captures);
        let foreign = SslContextBuilder::new(SslMethod::tls_client())
            .unwrap()
            .build();
        let mut other = Ssl::new(&foreign).unwrap();
        assert_eq!(
            other.set_bound_session(&snapshot),
            Err(BoundSessionError::ForeignContext)
        );
        assert!(other.session().is_none());
        // Current context now matches the snapshot, but original context does
        // not: a guard based only on ssl_context() would incorrectly accept.
        other.set_ssl_context(&context).unwrap();
        assert_eq!(
            other.set_bound_session(&snapshot),
            Err(BoundSessionError::ContextChanged)
        );
        assert!(other.session().is_none());
        let mut original = Ssl::new(&context).unwrap();
        original.set_bound_session(&snapshot).unwrap();
        let installed = original.session().unwrap().to_der().unwrap();
        original.set_ssl_context(&foreign).unwrap();
        assert_eq!(
            original.set_bound_session(&snapshot),
            Err(BoundSessionError::ContextChanged)
        );
        assert_eq!(original.session().unwrap().to_der().unwrap(), installed);
        // Restoring the original context is allowed; the API checks current
        // identity and original ownership, not a historical context-swap bit.
        original.set_ssl_context(&context).unwrap();
        original.set_bound_session(&snapshot).unwrap();
    }

    #[test]
    fn callback_refuses_snapshots_from_a_currently_swapped_context() {
        let captures = Arc::new(Mutex::new(Vec::new()));
        let original = client_context(SslVersion::TLS1_2, &captures, Arc::new(AtomicUsize::new(0)));
        let mut builder = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
        builder
            .set_max_proto_version(Some(SslVersion::TLS1_2))
            .unwrap();
        let changed = builder.build();
        let (address, worker) = peer(SslVersion::TLS1_2, 1);
        let mut ssl = Ssl::new(&original).unwrap();
        ssl.set_ssl_context(&changed).unwrap();
        connect(ssl, address, true);
        worker.join().unwrap().unwrap();
        let captured = captures.lock().unwrap();
        assert!(!captured.is_empty());
        assert!(captured
            .iter()
            .all(|result| matches!(result, Err(BoundSessionError::ContextChanged))));
    }

    #[test]
    fn retained_snapshots_keep_original_context_alive_without_callback_cycle() {
        let captures = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicUsize::new(0));
        let context = client_context(SslVersion::TLS1_2, &captures, dropped.clone());
        let (address, worker) = peer(SslVersion::TLS1_2, 1);
        connect(Ssl::new(&context).unwrap(), address, true);
        worker.join().unwrap().unwrap();
        let snapshot = captured_session(&captures);
        drop(context);
        drop(captures);
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "snapshot must retain original context"
        );
        let clone = snapshot.clone();
        drop(snapshot);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(clone);
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "weak cache capture must release context and callback"
        );
    }

    #[test]
    fn missing_original_slot_and_wrong_context_never_reach_der_installation() {
        initialize();
        // This test deliberately exercises existing unsafe raw interop inside
        // the vendor unit module; runtime and public extension APIs stay safe.
        let context = SslContextBuilder::new(SslMethod::tls_client())
            .unwrap()
            .build();
        let wrong = SslContextBuilder::new(SslMethod::tls_client())
            .unwrap()
            .build();
        let malformed = ContextBoundSession {
            inner: Arc::new(super::super::ContextBoundSessionInner {
                der: b"not a session".to_vec().into_boxed_slice(),
                context: context.clone(),
            }),
        };
        let mut foreign = Ssl::new(&wrong).unwrap();
        assert_eq!(
            foreign.set_bound_session(&malformed),
            Err(BoundSessionError::ForeignContext)
        );
        let raw = unsafe { ffi::SSL_new(context.as_ptr()) };
        assert!(!raw.is_null());
        let mut missing = unsafe { Ssl::from_ptr(raw) };
        assert_eq!(
            missing.set_bound_session(&malformed),
            Err(BoundSessionError::MissingOriginalContext)
        );
        assert!(missing.session().is_none());
        let mut same = Ssl::new(&context).unwrap();
        assert_eq!(
            same.set_bound_session(&malformed),
            Err(BoundSessionError::DecodeFailed)
        );
        assert!(same.session().is_none());
    }

    #[test]
    fn concurrent_installations_decode_distinct_native_sessions() {
        let captures = Arc::new(Mutex::new(Vec::new()));
        let context = client_context(SslVersion::TLS1_2, &captures, Arc::new(AtomicUsize::new(0)));
        let (address, worker) = peer(SslVersion::TLS1_2, 1);
        connect(Ssl::new(&context).unwrap(), address, true);
        worker.join().unwrap().unwrap();
        let snapshot = captured_session(&captures);
        let attempts: Vec<_> = (0..2)
            .map(|_| {
                let context = context.clone();
                let snapshot = snapshot.clone();
                thread::spawn(move || {
                    let mut ssl = Ssl::new(&context).unwrap();
                    ssl.set_bound_session(&snapshot).unwrap();
                    ssl
                })
            })
            .collect();
        let mut connections: Vec<_> = attempts
            .into_iter()
            .map(|attempt| attempt.join().unwrap())
            .collect();
        assert_ne!(
            connections[0].session().unwrap().as_ptr(),
            connections[1].session().unwrap().as_ptr()
        );
        drop(connections.remove(0));
        assert!(connections[0].session().unwrap().to_der().is_ok());
    }

    #[test]
    fn store_clones_share_one_snapshot_and_outlive_original_owners() {
        initialize();
        let certificate = X509::from_pem(ROOT_CERT).unwrap();
        let expected = certificate.to_der().unwrap();
        let mut builder = X509StoreBuilder::new().unwrap();
        builder.add_cert(certificate).unwrap();
        let store = builder.build();
        let shared = store.try_clone().unwrap();
        assert_eq!(store.as_ptr(), shared.as_ptr());
        let mut first = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
        first.set_cert_store(store.try_clone().unwrap());
        let first = first.build();
        let mut second = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
        second.set_cert_store(shared);
        let second = second.build();
        assert_eq!(first.cert_store().as_ptr(), second.cert_store().as_ptr());
        drop(store);
        drop(first);
        let retained = second.cert_store().try_clone().unwrap();
        drop(second);
        let certificates = retained.all_certificates();
        assert_eq!(certificates.len(), 1);
        assert_eq!(certificates[0].to_der().unwrap(), expected);
        drop(retained); // final owned up_ref is released exactly once
    }
}
