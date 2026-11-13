//! The `hearth` command. It takes one option, the data folder. Everything else is set in the
//! web UI and kept in the database.

use std::{
    io::IsTerminal,
    path::PathBuf,
    process::ExitCode,
};

use hearth::{Options, auth, db, paths::Paths};

const USAGE: &str = "\
Hearth, the self-hosted server for Bonfire.

Usage:
  hearth [--data <folder>]                         Start the Hearth
  hearth [--data <folder>] reset-password <user>   Give an account a new password
  hearth update                                    Fetch the newest version of Hearth
  hearth --version

  --data <folder>   Where the Hearth keeps its database, saves and backups.
                    Defaults to a folder named data next to the program.

There is no config file. Open the address the Hearth prints and set it up in the browser.";

enum Command {
    Serve,
    ResetPassword(String),
    Update,
}

fn main() -> ExitCode {
    let (data, command) = match parse(std::env::args().skip(1).collect()) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // The local time zone has to be read before any other thread exists.
    let utc_offset = time::UtcOffset::current_local_offset().map_or(0, |offset| offset.whole_seconds());
    hearth::update::clean_up();
    let runtime = tokio::runtime::Runtime::new().expect("could not start the async runtime");
    let result = runtime.block_on(async {
        match command {
            Command::Serve => serve(data, utc_offset).await,
            Command::ResetPassword(user) => reset_password(data, &user).await,
            Command::Update => update().await,
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Hearth could not continue: {err}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: Vec<String>) -> Result<Option<(PathBuf, Command)>, String> {
    let mut data = None;
    let mut rest = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data" => data = Some(PathBuf::from(args.next().ok_or("--data needs a folder.")?)),
            "--help" | "-h" | "help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--version" | "-V" => {
                println!("hearth {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            other => match other.strip_prefix("--data=") {
                Some(folder) => data = Some(PathBuf::from(folder)),
                None if other.starts_with('-') => return Err(format!("Hearth has no option {other}.")),
                None => rest.push(arg),
            },
        }
    }
    let data = match data {
        Some(data) => data,
        None => {
            let program = std::env::current_exe().map_err(|err| format!("Could not find the program's own folder: {err}"))?;
            program.parent().map(|dir| dir.join("data")).ok_or("Could not find the program's own folder.")?
        }
    };
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    let command = match rest.as_slice() {
        [] => Command::Serve,
        ["reset-password", user] => Command::ResetPassword(user.to_string()),
        ["reset-password", ..] => return Err("reset-password needs one username.".into()),
        ["update"] => Command::Update,
        [other, ..] => return Err(format!("Hearth has no command {other}.")),
    };
    Ok(Some((data, command)))
}

async fn serve(data: PathBuf, utc_offset: i32) -> Result<(), hearth::StartError> {
    let paths = Paths::create(&data)?;
    let _log = logging(&paths);
    let interactive = std::io::stdout().is_terminal();
    let running = hearth::run(Options { open_browser: interactive, utc_offset, ..Options::new(data) }).await?;

    let port = running.addr.port();
    let settings = running.state.settings();
    println!("Hearth {}", env!("CARGO_PKG_VERSION"));
    println!("  Data folder   {}", paths.root().display());
    match &running.setup_code {
        Some(code) => {
            let host = hearth::settings::lan_ip().map(|ip| ip.to_string()).unwrap_or_else(|| "localhost".into());
            println!("  Setup code    {code}");
            println!();
            println!("This Hearth has no owner yet. Open http://{host}:{port} in a browser and enter the setup code.");
            println!("On this machine, http://localhost:{port} works too.");
            if interactive {
                let _ = open::that(format!("http://localhost:{port}"));
            }
        }
        None => {
            let scheme = if running.state.bound().is_some_and(|bound| bound.https) { "https" } else { "http" };
            println!("  Address       {scheme}://{}:{port}", settings.address);
            println!();
            println!("{} is running. Stop it with Ctrl+C.", settings.name);
        }
    }

    tokio::select! {
        _ = tokio::signal::ctrl_c() => println!("Stopping."),
        _ = running.stopped() => {}
    }
    running.stop().await;
    Ok(())
}

/// Readable lines on the console, JSON lines in `<data>/logs`, one file per day, two weeks kept.
fn logging(paths: &Paths) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{Layer, filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

    let levels = || Targets::new().with_default(tracing::Level::WARN).with_target("hearth", tracing::Level::INFO);
    // Colours are for a terminal. In a file or a service's journal they would be noise.
    let console = tracing_subscriber::fmt::layer().compact().with_target(false).with_ansi(std::io::stdout().is_terminal()).with_filter(levels());
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("hearth")
        .filename_suffix("log")
        .max_log_files(14)
        .build(paths.dir("logs"));
    match file {
        Ok(file) => {
            let (writer, guard) = tracing_appender::non_blocking(file);
            let file = tracing_subscriber::fmt::layer().json().with_writer(writer).with_filter(levels());
            tracing_subscriber::registry().with(console).with(file).init();
            Some(guard)
        }
        Err(err) => {
            tracing_subscriber::registry().with(console).init();
            tracing::warn!(error = %err, "could not open the log folder, logging to the console only");
            None
        }
    }
}

/// The fallback for an owner who is locked out: run on the host, it sets a new random password
/// and signs the account out everywhere.
async fn reset_password(data: PathBuf, user: &str) -> Result<(), hearth::StartError> {
    let paths = Paths::create(&data)?;
    let pool = db::open(&paths.db()).await?;
    let password = format!("{}-{}", auth::setup_code(), auth::setup_code());
    let hash = auth::hash_password(&password);
    let changed = sqlx::query(
        "UPDATE users SET password_hash = ?, failed_logins = 0, locked_until = NULL WHERE username = ? AND removed_at IS NULL",
    )
    .bind(hash)
    .bind(user)
    .execute(&pool)
    .await?;
    if changed.rows_affected() == 0 {
        return Err(format!("there is no account named {user} in {}", paths.db().display()).into());
    }
    sqlx::query("DELETE FROM sessions WHERE user_id = (SELECT id FROM users WHERE username = ?)").bind(user).execute(&pool).await?;
    db::log_activity(&pool, format!("The password of {user} was reset on the host")).await;
    pool.close().await;
    println!("New password for {user}: {password}");
    println!("{user} is signed out on every device. Sign in with this password and change it.");
    Ok(())
}

/// Fetches the newest release and puts it in place of this program. A Hearth that is
/// running keeps going with the old version until it is started again.
async fn update() -> Result<(), hearth::StartError> {
    use hearth::update;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let release = update::latest(update::DEFAULT_API).await?;
    let current = env!("CARGO_PKG_VERSION");
    if !update::newer_than_running(&release.version) {
        println!("Hearth {current} is the newest version.");
        return Ok(());
    }
    println!("Fetching Hearth {}.", release.version);
    let exe = std::env::current_exe()?;
    let folder = exe.parent().ok_or("could not find the program's own folder")?;
    let file = update::download(update::DEFAULT_API, &release, folder).await?;
    update::install(&file)?;
    println!("Hearth {} is in place. If the Hearth is running, stop it and start it again to use the new version.", release.version);
    Ok(())
}
