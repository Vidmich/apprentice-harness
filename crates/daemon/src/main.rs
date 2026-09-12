//! `harnessd` — the apprentice-harness daemon.
//!
//! M00-01: prints its version. Lifecycle, RPC serving and core hosting arrive
//! with M00-08.

const NAME: &str = "harnessd";

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("{NAME} {}", apprentice_core::VERSION);
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown argument: {other}"),
        None => {
            println!(
                "{NAME} {} (not yet serving; see tasks/M00-foundations/M00-08)",
                apprentice_core::VERSION
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_is_stable() {
        assert_eq!(super::NAME, "harnessd");
    }
}
