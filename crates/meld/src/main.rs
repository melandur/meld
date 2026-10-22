//! Meld: a visual diff and merge tool.

#[macro_use]
mod i18n;

mod app;
mod cli;
mod command;
mod comparison;
mod dialogs;
mod forge;
mod instance;
mod shared;
mod ui;
mod views;

use std::sync::atomic::Ordering;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let args = cli::parse(raw_args.iter().cloned()).unwrap_or_else(|e| e.exit());
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let launch = match cli::launch_from_args(args, &cwd) {
        Ok(launch) => launch,
        Err(err) => {
            use clap::CommandFactory;
            eprintln!("{}", cli::Args::command().render_usage());
            eprint!("{}", tr!("Error: {}\n", err));
            std::process::exit(2);
        }
    };

    if launch.newtab
        && let Some(status) = instance::forward(&cwd, &raw_args)
    {
        std::process::exit(status);
    }

    let size = shared::Shared::initial_window_size();
    *app::LAUNCH.lock().expect("launch lock") = Some(launch);

    let result = iced::application(app::Meld::boot, app::Meld::update, app::Meld::view)
        .title(app::Meld::title)
        .theme(app::Meld::theme)
        .subscription(app::Meld::subscription)
        .window(app::Meld::window_settings(size))
        .antialiasing(true)
        .run();

    if let Err(err) = result {
        eprintln!("meld: {err}");
        std::process::exit(1);
    }
    std::process::exit(app::EXIT_STATUS.load(Ordering::SeqCst));
}
