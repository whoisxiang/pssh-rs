use ansi_term::Colour::{Green, Red};
use anyhow::{anyhow, Context};
use args::{AuthMethod, HostInfo};
use rayon::prelude::*;
use ssh2::Session;
use std::fs::metadata;
use std::fs::File;
use std::io::prelude::*;
use std::io::Write;
use std::net::TcpStream;
use std::os::unix::prelude::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::sync_channel;
use std::time::Duration;
use structopt::StructOpt;

mod args;

const DEFAULT_CONFIG_FPATH: &str = "./hosts.toml";
const DEFAULT_CONFIG_TMPL: &str = r#"username = "root"
password = "123456"
port = 22
timeout_ms = 10000
hosts = [
    "192.168.56.101",
    "192.168.56.102"
]

[nginx]
username = "root"
# SSH private key authentication. Use an absolute path or a path beginning with ~/.
private_key = "~/.ssh/id_rsa"
# Omit this option when the private key is not encrypted.
# private_key_passphrase = "your-passphrase"
port = 22
timeout_ms = 10000
hosts = [
    "192.168.57.101",
    "192.168.57.102"
]          
"#;

fn debug_log(debug: bool, scope: &str, detail: impl std::fmt::Display) {
    if debug {
        eprintln!("[DEBUG] {scope}: {detail}");
    }
}

fn main() -> anyhow::Result<()> {
    let args = args::CommandLineArgs::from_args();
    debug_log(args.debug, "main", format_args!("debug output enabled for command={}", args.command.name()));

    if let args::Command::Init = args.command {
        if std::fs::exists(DEFAULT_CONFIG_FPATH)? {
            println!("Save the following contents to a file (default to {}) and then", DEFAULT_CONFIG_FPATH);
            println!("using --config {} to use this config file.", DEFAULT_CONFIG_FPATH);
            println!();
            println!("{}", DEFAULT_CONFIG_TMPL);
        } else {
            std::fs::write(DEFAULT_CONFIG_FPATH, DEFAULT_CONFIG_TMPL)?;
            println!("default config template has been written to {}", DEFAULT_CONFIG_FPATH);
            println!("modify this file and using --config {} to use this config file", DEFAULT_CONFIG_FPATH)
        }
        return Ok(());
    }

    let hosts = args.get_hosts()?;
    debug_log(args.debug, "hosts", format_args!("loaded {} target(s)", hosts.len()));
    if hosts.is_empty() {
        eprintln!("no hosts found in config");
        return Ok(());
    }

    let (sender, receiver) = sync_channel(hosts.len());

    std::thread::scope(|s| {
        s.spawn({
            let hosts = hosts.clone();
            move || {
                print_main(args.keep_stable, &hosts, receiver);
            }
        });

        rayon::ThreadPoolBuilder::new().num_threads(args.num_threads).build_global().unwrap();
        hosts.par_iter().enumerate().for_each(|(index, host)| {
            let result = match &args.command {
                args::Command::Run { command } => run_command(host, command),
                args::Command::Send { source_fpath, target_fpath } => {
                    send_file(host, source_fpath, target_fpath, args.debug)
                }
                _ => {
                    panic!("shoud not come here")
                }
            };

            sender.send((index, host, result)).unwrap();
        });

        drop(sender);
    });

    Ok(())
}

enum Outcome {
    RunCommandOutcome(RunCommandOutcome),
    SendFileOutcome,
}

struct RunCommandOutcome {
    exit_status: i32,
    out: Vec<u8>,
    err: Vec<u8>,
}

fn run_command(host: &HostInfo, command: &str) -> anyhow::Result<Outcome> {
    let addr = format!("{}:{}", host.host, host.port);
    let tcp = TcpStream::connect_timeout(&addr.parse()?, Duration::from_millis(host.timeout_ms as u64))?;
    let mut sess = Session::new()?;
    sess.set_tcp_stream(tcp);
    sess.set_timeout(host.timeout_ms);
    sess.handshake()?;
    sess.set_timeout(0);
    authenticate(host, &sess)?;

    let mut channel = sess.channel_session()?;
    let _ = channel.setenv("PSSH_RS_IP", &host.host);
    let _ = channel.setenv("PSSH_RS_PORT", &host.port.to_string());
    channel.exec(command)?;

    let (mut out, mut err) = (vec![], vec![]);

    std::thread::scope(|s| {
        s.spawn(|| {
            if let Err(err) = channel.stream(0).take(1024 * 1024).read_to_end(&mut out) {
                println!("failed to read: {err}");
            }
        });
        s.spawn(|| {
            if let Err(err) = channel.stderr().take(1024 * 1024).read_to_end(&mut err) {
                println!("failed to read: {err}");
            }
        });
    });

    channel.wait_close()?;

    let exit_status = channel.exit_status()?;
    Ok(Outcome::RunCommandOutcome(RunCommandOutcome { exit_status, out, err }))
}

fn authenticate(host: &HostInfo, sess: &Session) -> anyhow::Result<()> {
    let addr = format!("{}:{}", host.host, host.port);

    match &host.auth {
        AuthMethod::Password { password } => sess
            .userauth_password(&host.username, password)
            .with_context(|| format!("authenticate user {} on {addr} with password", host.username)),
        AuthMethod::PrivateKey { private_key, passphrase } => sess
            .userauth_pubkey_file(&host.username, None, private_key, passphrase.as_deref())
            .with_context(|| {
                format!(
                    "authenticate user {} on {addr} with private key {}",
                    host.username,
                    private_key.display()
                )
            }),
    }
}

fn debug_send(debug: bool, host: &HostInfo, stage: &str, detail: impl std::fmt::Display) {
    if debug {
        eprintln!("[{}:{} DEBUG] send/{stage}: {detail}", host.host, host.port);
    }
}

fn resolve_remote_target(source_fpath: &Path, target_fpath: &Path) -> anyhow::Result<PathBuf> {
    if !target_fpath.as_os_str().is_empty() && !target_fpath.to_string_lossy().ends_with('/') {
        return Ok(target_fpath.to_path_buf());
    }

    let source_file_name = source_fpath
        .file_name()
        .ok_or_else(|| anyhow!("source path has no file name: {}", source_fpath.display()))?;
    Ok(target_fpath.join(source_file_name))
}

fn send_file(host: &HostInfo, source_fpath: &Path, target_fpath: &Path, debug: bool) -> anyhow::Result<Outcome> {
    let addr = format!("{}:{}", host.host, host.port);

    debug_send(
        debug,
        host,
        "start",
        format_args!(
            "source={} target={} user={} timeout={}ms",
            source_fpath.display(),
            target_fpath.display(),
            host.username,
            host.timeout_ms
        ),
    );

    let attr = metadata(source_fpath)
        .with_context(|| format!("stat local source file {}", source_fpath.display()))?;
    if !attr.is_file() {
        return Err(anyhow!("source is not a regular file: {}", source_fpath.display()));
    }

    let mode = attr.permissions().mode() & 0o777;
    let remote_target_fpath = resolve_remote_target(source_fpath, target_fpath)?;
    debug_send(debug, host, "source", format_args!("size={} mode={mode:03o}", attr.len()));
    debug_send(
        debug,
        host,
        "target",
        format_args!(
            "requested={} resolved={}",
            target_fpath.display(),
            remote_target_fpath.display()
        ),
    );

    let mut file =
        File::open(source_fpath).with_context(|| format!("open local source file {}", source_fpath.display()))?;
    debug_send(debug, host, "source", "opened local source file");

    let socket_addr = addr.parse().with_context(|| format!("parse SSH address {addr}"))?;
    let tcp = TcpStream::connect_timeout(&socket_addr, Duration::from_millis(host.timeout_ms as u64))
        .with_context(|| format!("connect to {addr} with timeout {}ms", host.timeout_ms))?;
    debug_send(debug, host, "transport", "TCP connection established");

    let mut sess = Session::new().with_context(|| format!("initialize SSH session for {addr}"))?;
    sess.set_tcp_stream(tcp);
    sess.set_timeout(host.timeout_ms);
    sess.handshake().with_context(|| format!("perform SSH handshake with {addr}"))?;
    debug_send(debug, host, "transport", "SSH handshake completed");

    sess.set_timeout(0);
    authenticate(host, &sess)?;
    debug_send(debug, host, "transport", "SSH authentication succeeded");

    debug_send(debug, host, "scp", "opening remote target");
    let mut remote_file = sess
        .scp_send(&remote_target_fpath, mode as i32, attr.len(), None)
        .with_context(|| format!("open remote SCP target {}", remote_target_fpath.display()))?;
    debug_send(debug, host, "scp", "remote target opened");

    let copied = std::io::copy(&mut file, &mut remote_file).with_context(|| {
        format!(
            "transfer local file {} to remote {}",
            source_fpath.display(),
            remote_target_fpath.display()
        )
    })?;
    if copied != attr.len() {
        return Err(anyhow!(
            "uploaded byte count mismatch for {}: expected {}, transferred {}",
            source_fpath.display(),
            attr.len(),
            copied
        ));
    }
    debug_send(debug, host, "scp", format_args!("transfer complete: {copied} bytes"));

    // Close the channel and wait for the whole content to be transferred
    remote_file
        .send_eof()
        .with_context(|| format!("send EOF to remote SCP target {}", remote_target_fpath.display()))?;
    remote_file
        .wait_eof()
        .with_context(|| format!("wait for remote SCP target {} to receive EOF", remote_target_fpath.display()))?;
    debug_send(debug, host, "scp", "remote target received EOF");

    remote_file
        .close()
        .with_context(|| format!("close remote SCP target {}", remote_target_fpath.display()))?;
    remote_file
        .wait_close()
        .with_context(|| format!("wait for remote SCP target {} to close", remote_target_fpath.display()))?;
    debug_send(debug, host, "finish", format_args!("uploaded {copied} bytes successfully"));

    Ok(Outcome::SendFileOutcome)
}

fn print_main(
    keep_stable: bool,
    hosts: &[args::HostInfo],
    receiver: std::sync::mpsc::Receiver<(usize, &args::HostInfo, anyhow::Result<Outcome>)>,
) {
    let mut results: Vec<_> = std::iter::repeat_with(|| None).take(hosts.len()).collect();
    let mut print_index: usize = 0;

    loop {
        let Ok((index, host, result)) = receiver.recv() else {
            break;
        };

        if !keep_stable {
            print_outcome(host, &result).unwrap();
            continue;
        }

        results[index] = Some((host, result));
        while print_index < results.len() {
            match results[print_index] {
                Some((host, ref result)) => {
                    print_outcome(host, result).unwrap();
                    print_index += 1;
                }
                None => {
                    break;
                }
            }
        }
    }
}

fn print_outcome(host: &HostInfo, result: &anyhow::Result<Outcome>) -> anyhow::Result<()> {
    let addr = format!("{}:{}", host.host, host.port);

    match result {
        Ok(Outcome::RunCommandOutcome(command_outcome)) => {
            print_command_outcome(&addr, command_outcome)?;
        }
        Ok(Outcome::SendFileOutcome) => {
            println!("{}", Green.paint(format!("[{addr} OK]")));
        }
        Err(err) => {
            println!("{}", Red.paint(format!("[{addr} ERROR: {err:#}]")));
        }
    }

    Ok(())
}

fn print_command_outcome(addr: &str, outcomd: &RunCommandOutcome) -> anyhow::Result<()> {
    if outcomd.exit_status == 0 {
        println!("{}", Green.paint(format!("[{addr} OK]")));
    } else {
        println!("{}", Red.paint(format!("[{addr} ERROR: exit with {}]", outcomd.exit_status)));
    }

    std::io::stdout().write_all(&outcomd.out)?;
    std::io::stdout().write_all(&outcomd.err)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_remote_target;
    use std::path::{Path, PathBuf};

    #[test]
    fn resolves_directory_target_to_source_file_name() {
        assert_eq!(
            resolve_remote_target(Path::new("./1.txt"), Path::new("/tmp/")).unwrap(),
            PathBuf::from("/tmp/1.txt")
        );
    }

    #[test]
    fn keeps_explicit_remote_file_path() {
        assert_eq!(
            resolve_remote_target(Path::new("./1.txt"), Path::new("/tmp/renamed.txt")).unwrap(),
            PathBuf::from("/tmp/renamed.txt")
        );
    }
}
