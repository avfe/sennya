use sennya::{cli, client, daemon};

#[cfg(feature = "gui")]
fn window() -> anyhow::Result<()> {
    #[cfg(windows)]
    unsafe {
        // Started from the Start menu, the console is ours alone and only gets in the way.
        // Started from a shell, it is the shell's and stays.
        use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
        let mut pids = [0u32; 2];
        if GetConsoleProcessList(pids.as_mut_ptr(), 2) == 1 {
            FreeConsole();
        }
    }
    sennya::gui::run()
}

#[cfg(not(feature = "gui"))]
fn window() -> anyhow::Result<()> {
    anyhow::bail!("this sennya was built without a window (daemon and CLI only); see `sennya --help`")
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("--window") | Some("--remote") => window(),
        Some("daemon") => daemon::run(),
        Some("proxy") => client::proxy().map_err(Into::into),
        Some("kill") => client::shutdown_daemon().map(|_| println!("sennya daemon stopped")).map_err(Into::into),
        Some("-h" | "--help") => {
            println!("{}", cli::HELP);
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("sennya {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(_) => {
            let cmd = args.remove(0);
            match cli::run(&cmd, args) {
                Ok(true) => Ok(()),
                Ok(false) => {
                    eprintln!("unknown command: {cmd}\n\n{}", cli::HELP);
                    std::process::exit(2);
                }
                Err(err) => Err(err),
            }
        }
    };
    if let Err(err) = result {
        eprintln!("sennya: {err:#}");
        std::process::exit(1);
    }
}
