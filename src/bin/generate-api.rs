fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            let path = hrrdarr::api_contract::output_path();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, hrrdarr::api_contract::render())?;
        }
        [flag] if flag == "--check" => hrrdarr::api_contract::check()?,
        [flag, path] if flag == "--check" => {
            hrrdarr::api_contract::check_file(std::path::Path::new(path))?
        }
        _ => return Err("Usage: generate-api [--check [artifact-path]]".into()),
    }
    Ok(())
}
