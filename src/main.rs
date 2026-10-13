mod action;
mod api;
mod app;
mod art;
mod auth;
mod config;
mod debug_log;
mod doctor;
mod lyrics;
mod media_controls;
mod paths;
mod player;
mod state;
mod ui;
mod visualizer;

use crate::action::{AppEvent, AuthStateEvent, Command};
use crate::app::{run_ui, App, PlayerCommand};
use crate::auth::AuthService;
use crate::config::Config;
use crate::media_controls::MediaControlsBridge;
use crate::paths::Paths;
use crate::player::PlayerService;
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "yt-tui", version, about = "YouTube Music in the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Enable debug logging
    #[arg(long)]
    debug: bool,
    /// Play a video id directly (debug)
    #[arg(long)]
    play: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Explains how to sign in (done inside the app)
    Login,
    /// Clear stored credentials
    Logout,
    /// Print environment diagnostics
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let paths = Arc::new(Paths::new()?);
    setup_logging(&paths, cli.debug)?;
    paths::cleanup_legacy(&paths);

    match cli.command {
        Some(Commands::Doctor) => {
            let report = doctor::doctor_report(&paths);
            println!("{report}");
            return Ok(());
        }
        Some(Commands::Logout) => {
            let mut auth = AuthService::new(paths.clone());
            auth.logout()?;
            println!("Signed out.");
            return Ok(());
        }
        Some(Commands::Login) => {
            let rt = tokio::runtime::Runtime::new()?;
            return rt.block_on(cli_login(paths));
        }
        None => {}
    }

    // macOS: pump CFRunLoop on main thread; run app on another thread
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown2 = shutdown.clone();
    let paths2 = paths.clone();
    let debug = cli.debug;
    let play = cli.play.clone();

    let handle = std::thread::Builder::new()
        .name("yt-tui-app".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let result = rt.block_on(async_main(paths2, debug, play));
            shutdown2.store(true, Ordering::Relaxed);
            result
        })?;

    media_controls::pump_run_loop(shutdown);

    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => anyhow::bail!("app thread panicked"),
    }
}

async fn cli_login(_paths: Arc<Paths>) -> Result<()> {
    anyhow::bail!(
        "Sign in from inside the app: press L (or P), then paste a cURL request or cookie header \
         copied from music.youtube.com."
    )
}

async fn async_main(paths: Arc<Paths>, _debug: bool, play: Option<String>) -> Result<()> {
    // Panic hook restores terminal
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableBracketedPaste
        );
        original(info);
    }));

    let config = Config::load(&paths)?;
    let auth = AuthService::new(paths.clone());
    let signed_in = auth.is_signed_in();
    let account_name = auth.session().and_then(|s| s.account_name.clone());
    let cookies = auth.cookie_header().map(|s| s.to_string());

    let (event_tx, event_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (player_cmd_tx, mut player_cmd_rx) = mpsc::unbounded_channel::<PlayerCommand>();

    // API worker
    let (api_cmd_tx, api_cmd_rx) = mpsc::unbounded_channel::<Command>();
    api::spawn_api_worker(paths.clone(), cookies.clone(), api_cmd_rx, event_tx.clone());
    tokio::spawn(auth::keepalive_loop(paths.clone(), api_cmd_tx.clone()));

    // Player service lives in a dedicated task
    let paths_p = paths.clone();
    let event_tx_p = event_tx.clone();
    let volume = config.playback.volume;
    let autoplay = config.playback.autoplay;
    let cache_mb = config.playback.cache_size_mb;
    let eq_params = player::EqParams {
        enabled: config.playback.eq_enabled,
        gains_db: config.playback.eq_gains,
    }
    .clamped();
    // Shared with the audio thread — UI reads this on each tick so the
    // visualizer moves even when no player commands are in flight.
    let viz_samples = player::SampleBuffer::default();
    let viz_samples_player = viz_samples.clone();
    let viz_samples_ui = viz_samples.clone();

    let player_cookies = cookies.clone();
    let player_handle = tokio::spawn(async move {
        let mut player = PlayerService::new(
            paths_p.clone(),
            volume,
            autoplay,
            cache_mb,
            eq_params,
            event_tx_p.clone(),
            viz_samples_player,
        );
        if player_cookies.is_some() {
            player.set_cookies(player_cookies).await;
        }
        let mut last_queue_len = usize::MAX;
        let mut last_cursor = usize::MAX;

        // Restore session paused
        if let Ok(session) = state::SessionState::load(&paths_p) {
            if !session.queue.is_empty() {
                let pos = session.position();
                let idx = session.index;
                let shuffle = session.shuffle;
                let repeat = session.repeat;
                let queue = session.queue;
                player.restore_session(queue, idx, pos, shuffle, repeat);
                if let Some(t) = player.current_track().cloned() {
                    let _ = event_tx_p.send(AppEvent::Player(action::PlayerEvent::Paused));
                    let _ = event_tx_p.send(AppEvent::Info(format!(
                        "Restored: {} (paused)",
                        t.title
                    )));
                }
            }
        }

        while let Some(cmd) = player_cmd_rx.recv().await {
            match cmd {
                PlayerCommand::PlayTrack(t) => player.play_track(t),
                PlayerCommand::PlayTracks { tracks, start } => {
                    player.play_tracks(tracks, start)
                }
                PlayerCommand::Toggle => player.toggle(),
                PlayerCommand::Play => player.engine.send(player::engine::EngineCmd::Play),
                PlayerCommand::Pause => player.engine.send(player::engine::EngineCmd::Pause),
                PlayerCommand::Next => player.next(),
                PlayerCommand::Previous => player.previous(),
                PlayerCommand::Seek(d) => player.seek(d),
                PlayerCommand::SeekRelative(s) => player.seek_relative(s),
                PlayerCommand::SetVolume(v) => player.set_volume(v),
                PlayerCommand::VolumeDelta(d) => player.volume_delta(d),
                PlayerCommand::Mute => player.toggle_mute(),
                PlayerCommand::ToggleShuffle => player.toggle_shuffle(),
                PlayerCommand::CycleRepeat => player.cycle_repeat(),
                PlayerCommand::AppendRadio(t) => player.append_radio(t),
                PlayerCommand::HandleEnded => player.handle_ended(),
                PlayerCommand::AddToQueue(t) => {
                    player.queue.add(t);
                }
                PlayerCommand::PlayNext(t) => player.queue.play_next(t),
                PlayerCommand::RemoveAt(i) => {
                    let _ = player.queue.remove_at_play_order(i);
                }
                PlayerCommand::ClearUpcoming => player.queue.clear_upcoming(),
                PlayerCommand::MoveQueue { from, to } => player.queue.move_play_order(from, to),
                PlayerCommand::JumpTo(i) => {
                    player.queue.jump_to_play_order(i);
                    player.start_current(std::time::Duration::ZERO);
                }
                PlayerCommand::RestoreSession {
                    tracks,
                    index,
                    position,
                    shuffle,
                    repeat,
                } => player.restore_session(tracks, index, position, shuffle, repeat),
                PlayerCommand::Shutdown => {
                    player.shutdown();
                    break;
                }
                PlayerCommand::SetEq(params) => player.set_eq(params),
                PlayerCommand::SetCookies(c) => player.set_cookies(c).await,
            }
            // Sync queue in *play* order so UI cursor / JumpTo match what plays
            // (storage order + play-order cursor desynced under shuffle/radio).
            let tracks: Vec<_> = player
                .queue
                .tracks_in_play_order()
                .into_iter()
                .cloned()
                .collect();
            let cursor = player.queue.cursor();
            if tracks.len() != last_queue_len || cursor != last_cursor {
                last_queue_len = tracks.len();
                last_cursor = cursor;
                let _ = event_tx_p.send(AppEvent::QueueSnapshot { tracks, cursor });
            }
        }
    });

    // Command dispatcher
    let event_tx_c = event_tx.clone();
    let paths_c = paths.clone();
    let api_cmd_tx2 = api_cmd_tx.clone();
    let player_cmd_tx_c = player_cmd_tx.clone();
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::ValidateCookies {
                    cookies,
                    visitor_id,
                } => {
                    let paths = paths_c.clone();
                    let tx = event_tx_c.clone();
                    let api = api_cmd_tx2.clone();
                    tokio::spawn(async move {
                        match auth::validate_and_save_with_visitor(
                            paths,
                            cookies,
                            None,
                            None,
                            visitor_id,
                        )
                        .await
                        {
                            Ok(session) => {
                                let _ = api.send(Command::ApplyAuth(session.clone()));
                                let _ = tx.send(AppEvent::AuthState(AuthStateEvent::SignedIn(
                                    session,
                                )));
                            }
                            Err(e) => {
                                let _ = tx.send(AppEvent::AuthState(AuthStateEvent::Failed(
                                    e.to_string(),
                                )));
                            }
                        }
                    });
                }
                Command::ApplyAuth(ref session) => {
                    let _ = player_cmd_tx_c
                        .send(PlayerCommand::SetCookies(Some(session.cookies.clone())));
                    let _ = api_cmd_tx2.send(cmd);
                }
                Command::ClearAuth => {
                    let _ = player_cmd_tx_c.send(PlayerCommand::SetCookies(None));
                    let _ = api_cmd_tx2.send(Command::ClearAuth);
                }
                Command::Logout => {
                    let mut a = AuthService::new(paths_c.clone());
                    let _ = a.logout();
                    // Drop live Innertube + stream-resolver cookies immediately.
                    let _ = api_cmd_tx2.send(Command::ClearAuth);
                    let _ = player_cmd_tx_c.send(PlayerCommand::SetCookies(None));
                    let _ = event_tx_c.send(AppEvent::AuthState(AuthStateEvent::LoggedOut));
                }
                Command::FetchArt { url, video_id } => {
                    let paths = paths_c.clone();
                    let tx = event_tx_c.clone();
                    tokio::spawn(art::fetch_art(paths, video_id, url, tx));
                }
                Command::LoadLyrics {
                    video_id,
                    browse_id,
                    track,
                } => {
                    let paths = paths_c.clone();
                    let tx = event_tx_c.clone();
                    let api = api_cmd_tx2.clone();
                    tokio::spawn(async move {
                        lyrics::fetch_lyrics(paths, track.clone(), browse_id.clone(), tx).await;
                        if browse_id.is_some() {
                            let _ = api.send(Command::LoadLyrics {
                                video_id,
                                browse_id,
                                track,
                            });
                        }
                    });
                }
                // Forward API commands
                other => {
                    let _ = api_cmd_tx2.send(other);
                }
            }
        }
    });

    // Re-validate cached cookies on launch — a saved username can outlive a dead session,
    // which otherwise leaves Library empty while still showing the account name.
    if let Some(cookie) = cookies.clone() {
        let paths_v = paths.clone();
        let tx_v = event_tx.clone();
        let api_v = api_cmd_tx.clone();
        tokio::spawn(async move {
            match auth::validate_and_save(paths_v.clone(), cookie, None, None).await {
                Ok(session) => {
                    let _ = api_v.send(Command::ApplyAuth(session.clone()));
                    if let Some(name) = session.account_name.clone() {
                        let _ = tx_v.send(AppEvent::Account(Some(crate::api::model::Account {
                            name,
                            channel_id: None,
                            thumbnails: vec![],
                        })));
                    }
                }
                Err(e) => {
                    // Rotate the session's cookies like a browser would before giving up.
                    match auth::recover_session(paths_v.clone()).await {
                        Some(session) => {
                            let _ = api_v.send(Command::ApplyAuth(session.clone()));
                            let _ = tx_v.send(AppEvent::AuthState(AuthStateEvent::SignedIn(session)));
                        }
                        None => {
                            let _ = tx_v.send(AppEvent::AuthState(AuthStateEvent::SessionExpired(
                                e.to_string(),
                            )));
                        }
                    }
                }
            }
        });
    }

    let app = App::new(
        paths.clone(),
        config,
        cmd_tx.clone(),
        player_cmd_tx.clone(),
        signed_in,
        account_name,
    );

    // Optional debug play
    if let Some(id) = play {
        let track = api::model::Track {
            video_id: id,
            title: "Direct play".into(),
            artists: vec![],
            album: None,
            duration: None,
            thumbnails: vec![],
            explicit: false,
            liked: None,
            from_radio: false,
        };
        let _ = player_cmd_tx.send(PlayerCommand::PlayTrack(track));
    }

    let media = MediaControlsBridge::new();
    let sample_provider: Arc<dyn Fn(&mut Vec<f32>) + Send + Sync> =
        Arc::new(move |out: &mut Vec<f32>| {
            viz_samples_ui.snapshot(out);
        });

    let result = run_ui(app, event_rx, cmd_tx, media, sample_provider).await;

    let _ = player_cmd_tx.send(PlayerCommand::Shutdown);
    let _ = player_handle.await;
    result
}

fn setup_logging(paths: &Paths, debug: bool) -> Result<()> {
    let file_appender = tracing_appender::rolling::daily(paths.logs_dir(), "yt-tui.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    // Leak guard so it lives for process lifetime
    std::mem::forget(_guard);

    let level = if debug { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();
    Ok(())
}
