use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let schema_path = out_dir.join("schema.sdl");
    std::fs::write(&schema_path, fuel_core_client::SCHEMA_SDL).unwrap();

    cynic_codegen::register_schema("fuelcore")
        .from_sdl_file(schema_path)
        .unwrap()
        .as_default()
        .unwrap();
}
