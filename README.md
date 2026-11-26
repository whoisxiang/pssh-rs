# pssh-rs

`pssh-rs` is a parallel SSH command and file transfer tool written in Rust.

It can:

- run a command on multiple hosts concurrently;
- select a host group from a TOML configuration file;
- upload a local file to multiple hosts over SCP;
- authenticate with a password or an SSH private key;
- keep output in configuration order with `--keep_stable`.

## Install

Install the published binary with Cargo:

```bash
cargo install pssh-rs
```

Or build it from source:

```bash
cargo build --release
```

The resulting binary is `target/release/pssh-rs`.

To build a static Linux binary:

```bash
sudo apt install musl-tools -y
rustup target add x86_64-unknown-linux-musl
cargo build --target=x86_64-unknown-linux-musl --release
```

## Quick start

Generate a configuration template:

```bash
pssh-rs init
```

This writes `hosts.toml` in the current directory. Edit the hosts, username,
and authentication settings, then run:

```bash
pssh-rs --config=./hosts.toml --num_threads=10 run 'date'
```

Run on the hosts in the `[nginx]` section:

```bash
pssh-rs --config=./hosts.toml --num_threads=10 -s nginx run 'date'
```

Send a file to every selected host:

```bash
pssh-rs --config=./hosts.toml send ./hello.txt /tmp/
```

As with `scp`, a destination ending in `/` keeps the local file name. A
destination without a trailing slash is used as the remote file path:

```bash
pssh-rs --config=./hosts.toml send ./hello.txt /tmp/hello-renamed.txt
```

## Configuration

The top-level configuration is used when `--section` is omitted. A named
section such as `[nginx]` is selected with `-s nginx` or `--section nginx`.
Each section is independent; fields are not inherited from the top level.

Common options:

| Option | Default | Description |
| --- | --- | --- |
| `username` | `root` | SSH username |
| `password` | none | Password for password authentication |
| `private_key` | none | Path to an SSH private key |
| `private_key_passphrase` | none | Passphrase for an encrypted private key |
| `port` | `22` | SSH port |
| `timeout_ms` | `3000` | TCP connect and SSH operation timeout in milliseconds |
| `hosts` | required | Array of host names or IP addresses |

Every top-level table or selected section must define either `password` or
`private_key`. When both are present, `private_key` takes precedence.

### Password authentication

```toml
username = "deploy"
password = "change-me"
port = 22
timeout_ms = 10000
hosts = [
    "192.0.2.10",
    "192.0.2.11",
]
```

### SSH key authentication

Create a key if you do not already have one, and install its public key on the
remote host:

```bash
ssh-keygen -t rsa -b 4096 -f ~/.ssh/pssh_rsa
ssh-copy-id -i ~/.ssh/pssh_rsa.pub deploy@192.0.2.10
chmod 600 ~/.ssh/pssh_rsa
```

Then configure the private key in `hosts.toml`:

```toml
username = "deploy"
private_key = "~/.ssh/pssh_rsa"
port = 22
timeout_ms = 10000
hosts = [
    "192.0.2.10",
    "192.0.2.11",
]
```

A path beginning with `~/` is expanded from `$HOME`. Other relative paths are
resolved from the process working directory.

For an encrypted private key, add its passphrase:

```toml
username = "deploy"
private_key = "~/.ssh/encrypted_key"
private_key_passphrase = "key-passphrase"
hosts = ["192.0.2.10"]
```

If the key is not encrypted, omit `private_key_passphrase`.

The key must be readable by the current process. Restrict permissions so other
users cannot read it:

```bash
chmod 600 ~/.ssh/pssh_rsa
```

The TOML file may contain a password or key passphrase in plain text, so it
should also be protected:

```bash
chmod 600 hosts.toml
```

Verify the key independently before using `pssh-rs`:

```bash
ssh -i ~/.ssh/pssh_rsa deploy@192.0.2.10 true
```

If libssh2 rejects a key format, regenerate the private key in PEM format or
use an RSA key, then copy its public key to the remote host again.

### Host sections

Sections can use different credentials and host groups:

```toml
[production]
username = "deploy"
private_key = "/home/deploy/.ssh/production_rsa"
port = 22
timeout_ms = 10000
hosts = [
    "prod-01.example.com",
    "prod-02.example.com",
]

[staging]
username = "deploy"
password = "staging-password"
port = 2222
timeout_ms = 15000
hosts = [
    "staging-01.example.com",
]
```

Run against one group with:

```bash
pssh-rs --config=./hosts.toml -s production run 'uptime'
```

## Options

```text
pssh-rs [OPTIONS] <SUBCOMMAND>

Options:
  -c, --config <config>              TOML configuration file [default: ./hosts.toml]
  -s, --section <section>            Section in the TOML file
  -n, --num_threads <num_threads>    Number of worker threads [default: 1]
  -k, --keep_stable                  Keep output in configured host order
  -d, --debug                        Print detailed diagnostics while sending files
  -h, --help                         Print help
  -V, --version                      Print version
```

For SCP failures, `--debug` prints connection and upload stages to `stderr`.
The global flag can appear before or after the subcommand:

```bash
pssh-rs --debug --config=./hosts.toml send ./hello.txt /tmp/
pssh-rs --config=./hosts.toml send ./hello.txt /tmp/ -d
```

## Development

```bash
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
```
