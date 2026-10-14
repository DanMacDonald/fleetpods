//! Command client for the FleetPods daemon.

use std::env;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use fleetpods::linux::paths::SOCKET_NAME;
use fleetpods::panel::ipc;

fn main() {
    let mut args = env::args();
    let argv0 = args.next().unwrap_or_else(|| "fleetpods".to_string());
    let name = PathBuf::from(&argv0)
        .file_name()
        .and_then(|text| text.to_str())
        .unwrap_or("fleetpods")
        .to_string();
    let Some(command) = args.next() else {
        usage(&name);
        std::process::exit(1);
    };
    if command == "--help" || command == "-h" {
        usage(&name);
        return;
    }
    if command == "--version" || command == "-V" {
        println!("{name} {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let rest: Vec<_> = args.collect();
    let remove = if command == "fleet" && rest.first().map(String::as_str) == Some("remove") {
        let address = rest.get(1).and_then(|text| fleetpods::types::Address::parse(text));
        if rest.len() != 2 || address.is_none() {
            eprintln!("Usage: {name} fleet remove <Bluetooth address>");
            std::process::exit(1);
        }
        address
    } else {
        None
    };
    let command = if command == "fleet" && rest == ["reset"] {
        "fleet-reset".to_string()
    } else if command == "audio" && rest == ["reset"] {
        "audio-reset".to_string()
    } else if rest.is_empty() || remove.is_some() {
        command
    } else {
        usage(&name);
        std::process::exit(1);
    };
    let mut wire = command.clone();
    if let Some(address) = remove {
        wire = format!(
            "fleet:{}\n",
            serde_json::json!({"action": "remove", "address": address.display()})
        );
    } else if command == "fleet" {
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err()
            || !line.ends_with('\n')
            || line.len() > ipc::MAX_BYTES
        {
            eprintln!("Fleet expects one JSON line on stdin");
            std::process::exit(1);
        }
        wire = format!("fleet:{line}");
    }

    let Some(runtime) = env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) else {
        eprintln!("XDG_RUNTIME_DIR is unset; cannot locate the daemon socket");
        std::process::exit(1);
    };
    let path = PathBuf::from(runtime).join(SOCKET_NAME);
    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(_) => {
            eprintln!(
                "Could not connect to FleetPods daemon at {} (is it running?)",
                path.display()
            );
            std::process::exit(1);
        }
    };
    if stream.write_all(wire.as_bytes()).is_err() {
        eprintln!("Could not write to the daemon socket");
        std::process::exit(1);
    }
    if !ipc::cli_waits(&command) {
        return;
    }
    // Reset waits behind pending keyring writes and confirms durable deletion.
    let timeout = if command == "fleet-reset" || command == "fleet" { 120 } else { 1 };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(timeout)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if buf.len() > ipc::MAX_BYTES {
            break;
        }
    }
    if buf.is_empty() {
        eprintln!("Timed out waiting for a reply to {command}");
        std::process::exit(1);
    }
    if buf.starts_with(b"error:") {
        let _ = io::stderr().write_all(&buf);
        std::process::exit(1);
    }
    let _ = io::stdout().write_all(&buf);
}

fn usage(name: &str) {
    eprintln!(
        "\
Usage: {name} <command>
Commands:
  fleet               Read a fleet request as JSON from stdin
  fleet reset         Reset this machine's fleet; preserve AirPods pairing
  fleet remove ADDR   Remove one member from this machine's saved fleet
  status              Print one-line JSON status snapshot to stdout
  audio reset         Reopen the local AirPods audio output; preserve pairing
  noise:off           Request noise control off
  noise:anc           Request active noise cancellation
  noise:transparency  Request transparency
  noise:adaptive      Request adaptive noise control
  ear:off             Disable ear-detection pause
  ear:one             Pause when either bud is removed
  ear:both            Pause only when both buds are removed
  ca:on               Request conversation awareness on
  ca:off              Request conversation awareness off
  connect             Request the local seat
  reconnect-peer      Request the Apple-device reconnect
  activity            Submit fresh desktop activity
  handoff:release     Release local audio ownership
  adaptive:N          Request adaptive level 0-100
  onebud:on           Request one-bud noise cancellation on
  onebud:off          Request one-bud noise cancellation off"
    );
}
