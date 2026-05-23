//! Networking: the Wi-Fi SoftAP, the HTTP upload server, and the captive-portal
//! DNS responder.
//!
//! The device is its own access point - no router. A phone or laptop joins the
//! `JusticeTattoo` network; the captive-portal DNS points every hostname at the
//! device, so the browser opens the upload page. The page converts a GIF to a
//! JTM1 blob and POSTs it to `/upload`, which streams it into the `movie` flash
//! partition and reboots into the new movie.

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::http::server::{Configuration as HttpConfig, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, Configuration as WifiConfig, EspWifi,
};
use std::net::UdpSocket;
use std::thread;

use crate::movie::MovieWriter;

/// The SoftAP network name and password. The SSID carries the address to open,
/// so it shows up right in the phone's Wi-Fi list - a fallback for when the
/// captive portal does not pop. It must fit in 32 bytes; WPA2 needs a password
/// of >= 8 characters.
const SSID: &str = "JusticeTattoo 192.168.71.1";
const PASSWORD: &str = "justicetattoo";

/// The SoftAP gateway address. esp-idf-svc's default AP netif lives on the
/// 192.168.71.x subnet; this must match the address advertised in the SSID and
/// handed out by the captive-portal DNS.
const AP_IP: [u8; 4] = [192, 168, 71, 1];

/// The upload page, vendored into the firmware and served from flash.
const UPLOAD_PAGE: &str = include_str!("../web/index.html");

/// Holds the Wi-Fi and HTTP server alive for the life of the program; dropping
/// either tears the service down, so `main` keeps this value.
pub struct Net {
    _wifi: EspWifi<'static>,
    _server: EspHttpServer<'static>,
}

/// Bring up the SoftAP, the HTTP server, and the captive-portal DNS.
pub fn start(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
) -> Result<Net> {
    let wifi = start_softap(modem, sysloop, nvs)?;
    let server = start_http_server()?;
    spawn_captive_dns();
    Ok(Net {
        _wifi: wifi,
        _server: server,
    })
}

/// Configure Wi-Fi as a WPA2 SoftAP and start it.
fn start_softap(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
) -> Result<EspWifi<'static>> {
    let mut wifi = EspWifi::new(modem, sysloop, Some(nvs))?;
    wifi.set_configuration(&WifiConfig::AccessPoint(AccessPointConfiguration {
        ssid: SSID.try_into().expect("SSID fits in 32 bytes"),
        password: PASSWORD.try_into().expect("password fits in 64 bytes"),
        auth_method: AuthMethod::WPA2Personal,
        channel: 1,
        max_connections: 4,
        ..Default::default()
    }))?;
    wifi.start()?;
    log::info!(
        "wifi: SoftAP '{SSID}' up - join it and open http://{}.{}.{}.{}/",
        AP_IP[0],
        AP_IP[1],
        AP_IP[2],
        AP_IP[3],
    );
    Ok(wifi)
}

/// Start the HTTP server: the upload page on `/` (and any other path, for the
/// captive portal) and the streamed upload on `POST /upload`.
fn start_http_server() -> Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpConfig {
        // The upload handler keeps a 4 KB sector buffer on its stack.
        stack_size: 16 * 1024,
        // Let `/*` match every otherwise-unhandled path (captive portal).
        uri_match_wildcard: true,
        ..Default::default()
    })?;

    server.fn_handler("/", Method::Get, |req| -> Result<(), anyhow::Error> {
        let mut resp = req.into_response(
            200,
            Some("OK"),
            &[("Content-Type", "text/html; charset=utf-8")],
        )?;
        resp.write_all(UPLOAD_PAGE.as_bytes())?;
        Ok(())
    })?;

    // Captive-portal catch-all: any other path also shows the uploader, so the
    // phone's "sign in to network" prompt lands straight on the page.
    server.fn_handler("/*", Method::Get, |req| -> Result<(), anyhow::Error> {
        let mut resp = req.into_response(
            200,
            Some("OK"),
            &[("Content-Type", "text/html; charset=utf-8")],
        )?;
        resp.write_all(UPLOAD_PAGE.as_bytes())?;
        Ok(())
    })?;

    server.fn_handler("/upload", Method::Post, |mut req| -> Result<(), anyhow::Error> {
        log::info!("upload: receiving movie");
        let mut writer = MovieWriter::new()?;
        let mut buf = [0u8; 1024];
        let mut total = 0usize;
        loop {
            let n = req.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write(&buf[..n])?;
            total += n;
        }
        let written = writer.finish()?;
        log::info!("upload: {total} bytes received, {written} written; rebooting");

        let mut resp = req.into_ok_response()?;
        resp.write_all(b"OK")?;
        resp.flush()?;
        drop(resp);

        // Let the TCP response drain, then reboot into the new movie.
        // esp_restart() is `-> !`, so it is the handler's final expression.
        FreeRtos::delay_ms(1000);
        unsafe { esp_idf_svc::sys::esp_restart() }
    })?;

    log::info!("http: server listening on :80");
    Ok(server)
}

/// Spawn the captive-portal DNS thread: a UDP server on `:53` that answers
/// every query with the SoftAP address, so joining the network opens the page.
fn spawn_captive_dns() {
    let spawned = thread::Builder::new()
        .name("captive-dns".into())
        .stack_size(8 * 1024)
        .spawn(|| match UdpSocket::bind("0.0.0.0:53") {
            Ok(sock) => {
                log::info!("dns: captive portal responder up on :53");
                let mut buf = [0u8; 512];
                loop {
                    if let Ok((n, src)) = sock.recv_from(&mut buf) {
                        if let Some(reply) = dns_reply(&buf[..n]) {
                            let _ = sock.send_to(&reply, src);
                        }
                    }
                }
            }
            Err(e) => log::error!("dns: bind :53 failed: {e}"),
        });
    if let Err(e) = spawned {
        log::error!("dns: could not spawn responder thread: {e}");
    }
}

/// Build a DNS reply that points the queried name at the SoftAP address.
///
/// Echoes the question, flips the header to an authoritative answer, and appends
/// one A record. Every name resolves to the device - that is the whole point of
/// a captive portal.
fn dns_reply(query: &[u8]) -> Option<Vec<u8>> {
    // 12-byte header, and it must be a query (QR bit clear) with >= 1 question.
    if query.len() < 12 || query[2] & 0x80 != 0 {
        return None;
    }
    // Walk the QNAME labels to the end of the question section.
    let mut p = 12;
    while p < query.len() && query[p] != 0 {
        p += 1 + query[p] as usize;
    }
    p += 1 + 4; // zero-length label + QTYPE + QCLASS
    if p > query.len() {
        return None;
    }

    let mut reply = Vec::with_capacity(p + 16);
    reply.extend_from_slice(&query[..p]);
    reply[2] = 0x81; // QR=1, Opcode=0, AA=1
    reply[3] = 0x80; // RA=1, RCODE=0
    reply[6] = 0x00; // ANCOUNT hi
    reply[7] = 0x01; // ANCOUNT lo = 1
    reply[8..12].fill(0); // NSCOUNT + ARCOUNT = 0

    reply.extend_from_slice(&[0xC0, 0x0C]); // NAME -> pointer to offset 12
    reply.extend_from_slice(&[0x00, 0x01]); // TYPE  A
    reply.extend_from_slice(&[0x00, 0x01]); // CLASS IN
    reply.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // TTL 60 s
    reply.extend_from_slice(&[0x00, 0x04]); // RDLENGTH 4
    reply.extend_from_slice(&AP_IP); // RDATA
    Some(reply)
}
