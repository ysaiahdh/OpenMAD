#[cfg(windows)]
fn main() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");

    let ini_content = std::fs::read_to_string("version.ini").unwrap_or_default();
    let mut name = "OpenMAD".to_string();
    let mut version = "1.1.4".to_string();
    let mut publisher = "Acciaw".to_string();

    for line in ini_content.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "name" => name = v.trim().to_string(),
                "version" => version = v.trim().to_string(),
                "publisher" => publisher = v.trim().to_string(),
                _ => {}
            }
        }
    }

    res.set("ProductName", &name);
    res.set(
        "FileDescription",
        "Pilote et utilitaire natif Windows pour claviers magnétiques MAD68",
    );
    res.set("CompanyName", &publisher);
    res.set("LegalCopyright", &format!("Copyright (c) 2026 {}", publisher));

    let parts: Vec<u16> = version
        .split('.')
        .filter_map(|p| p.parse::<u16>().ok())
        .collect();
    let major = *parts.first().unwrap_or(&1) as u64;
    let minor = *parts.get(1).unwrap_or(&0) as u64;
    let patch = *parts.get(2).unwrap_or(&0) as u64;
    let build = *parts.get(3).unwrap_or(&0) as u64;
    let version_u64 = (major << 48) | (minor << 32) | (patch << 16) | build;
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, version_u64);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, version_u64);

    if let Err(e) = res.compile() {
        eprintln!("Avertissement: compilation des ressources Windows ignorée ({e})");
    }
}

#[cfg(not(windows))]
fn main() {}

