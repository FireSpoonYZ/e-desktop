mod logic;
#[cfg(windows)]
mod native;

fn main() {
    if let Err(error) = entry() {
        eprintln!("window-handoff-probe: {error}");
        std::process::exit(1);
    }
}

fn entry() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match logic::parse(&args)? {
        logic::Action::Help => println!("{}", logic::HELP),
        logic::Action::SelfTest => {
            logic::self_check()?;
            #[cfg(windows)]
            native::memory_check()?;
            println!(
                "Pure logic / memory-DC checks passed; no HWND created; not visual validation."
            );
        }
        logic::Action::Plan(config) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?
            );
            println!(
                "Not executed: --run absent. No native preflight, window, capture or desktop mutation."
            );
        }
        logic::Action::Run(config) => {
            #[cfg(windows)]
            return native::run(config);
            #[cfg(not(windows))]
            {
                let _ = config;
                return Err("--run requires Windows".into());
            }
        }
    }
    Ok(())
}
