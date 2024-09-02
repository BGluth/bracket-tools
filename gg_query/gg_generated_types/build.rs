fn main() -> anyhow::Result<()> {
    println!("cargo::rerun-if-changed=gg_info/schema.graphql");

    cynic_codegen::register_schema("start_gg")
        .from_sdl_file("gg_info/schema.graphql")?
        .as_default()?;

    Ok(())
}
