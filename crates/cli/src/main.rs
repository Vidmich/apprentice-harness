//! `harness` — the apprentice-harness CLI.
//!
//! M00-01: prints its version. The command tree arrives with M00-09.
//! This binary must never depend on `apprentice-core`.

const NAME: &str = "harness";

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("{NAME} {}", apprentice_client::VERSION);
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown argument: {other}"),
        None => {
            println!(
                "{NAME} {} (no commands yet; see tasks/M00-foundations/M00-09)",
                apprentice_client::VERSION
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_is_stable() {
        assert_eq!(super::NAME, "harness");
    }
}
