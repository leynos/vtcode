#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]

#[cfg(not(feature = "schema"))]
fn main() {
    eprintln!("The schema_dump example requires --features schema");
    std::process::exit(1);
}

#[cfg(feature = "schema")]
fn main() -> anyhow::Result<()> {
    let schema = diapason_config::diapason_config_schema_json()?;
    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}
