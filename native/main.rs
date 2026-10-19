#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod config;
mod connection;
mod cursor;
mod documents;
mod filesystem;
mod imaging;
mod mcp;
mod pdf;
mod pdf_page;
mod service;
#[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
mod tray;
mod vault;
mod walk;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = args
        .windows(2)
        .find(|v| v[0] == "--config")
        .map(|v| std::path::PathBuf::from(&v[1]))
        .unwrap_or_else(config::default_path);
    match args.first().map(String::as_str).unwrap_or("tray") {
        "serve" => mcp::serve(config_path),
        "worker" => service::worker(&config_path),
        "background" => service::background(&config_path),
        "init" => {
            config::initialize(&config_path)?;
            println!("{}", config_path.display());
            Ok(())
        }
        "check" => {
            let c = config::Config::load(&config_path)?;
            c.validate()?;
            println!("{} folders configured", c.roots.len());
            Ok(())
        }
        "status" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&service::health(&config_path))?
            );
            Ok(())
        }
        "tray" => {
            #[cfg(all(feature = "desktop", any(target_os = "macos", target_os = "windows")))]
            {
                tray::run(config_path)
            }
            #[cfg(not(all(feature = "desktop", any(target_os = "macos", target_os = "windows"))))]
            {
                anyhow::bail!(
                    "Desktop UI is available on macOS and Windows; use serve for headless operation"
                )
            }
        }
        "--version" | "version" => {
            println!("Strigov Local MCP {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            println!("Strigov Local MCP: tray | serve | init | check | status [--config PATH]");
            Ok(())
        }
    }
}
