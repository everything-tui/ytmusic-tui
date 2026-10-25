//! Challenge solver for signature-ciphered stream URLs (WEB_REMIX).
//!
//! Runs yt-dlp's EJS solver (vendored in `assets/ejs`) on an embedded QuickJS.
//! Preprocessing a player script takes ~2s, so the result is cached on disk per
//! player version and kept loaded on a dedicated thread; solves are then cheap.

use anyhow::{anyhow, bail, Context as _, Result};
use rquickjs::{Context, Function, Object, Runtime};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, Mutex};
use tracing::{info, warn};

const SOLVER_LIB: &str = include_str!("../../assets/ejs/lib.min.js");
const SOLVER_CORE: &str = include_str!("../../assets/ejs/core.min.js");

const IFRAME_API_URL: &str = "https://www.youtube.com/iframe_api";
/// How long a looked-up player version is trusted before checking for a newer one.
const PLAYER_TTL: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone)]
pub struct PlayerScript {
    pub id: String,
    /// `signatureTimestamp` the player request must carry so ciphers match this script.
    pub sts: u64,
}

#[derive(Debug, Default)]
pub struct Solved {
    pub sig: HashMap<String, String>,
    pub n: HashMap<String, String>,
}

struct Job {
    player_id: String,
    script: PathBuf,
    preprocessed: PathBuf,
    sig: Vec<String>,
    n: Vec<String>,
    reply: oneshot::Sender<Result<Solved>>,
}

#[derive(Clone)]
pub struct Jsc {
    http: reqwest::Client,
    cache_dir: PathBuf,
    current: Arc<Mutex<Option<(PlayerScript, Instant)>>>,
    jobs: mpsc::Sender<Job>,
}

impl Jsc {
    pub fn new(http: reqwest::Client, cache_dir: PathBuf) -> Self {
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("yt-tui-jsc".into())
            .spawn(move || solver_thread(rx))
            .expect("spawn jsc thread");
        Self {
            http,
            cache_dir,
            current: Arc::new(Mutex::new(None)),
            jobs,
        }
    }

    /// The current player version, downloading its script if it isn't cached.
    pub async fn player(&self) -> Result<PlayerScript> {
        let mut current = self.current.lock().await;
        if let Some((script, at)) = current.as_ref() {
            if at.elapsed() < PLAYER_TTL {
                return Ok(script.clone());
            }
        }
        let id = self.fetch_player_id().await?;
        let path = self.script_path(&id);
        if !path.exists() {
            let url = format!("https://www.youtube.com/s/player/{id}/player_ias.vflset/en_US/base.js");
            let body = self
                .http
                .get(&url)
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .with_context(|| format!("download player {id}"))?
                .bytes()
                .await
                .context("player script body")?;
            std::fs::create_dir_all(&self.cache_dir)?;
            write_atomic(&path, &body)?;
            info!("downloaded player script {id} ({} bytes)", body.len());
            self.remove_other_versions(&id);
        }
        let source = std::fs::read_to_string(&path).context("read player script")?;
        let sts = extract_sts(&source).with_context(|| format!("no signatureTimestamp in player {id}"))?;
        let script = PlayerScript { id, sts };
        *current = Some((script.clone(), Instant::now()));
        Ok(script)
    }

    pub async fn solve(&self, player: &PlayerScript, sig: Vec<String>, n: Vec<String>) -> Result<Solved> {
        let (reply, rx) = oneshot::channel();
        self.jobs
            .send(Job {
                player_id: player.id.clone(),
                script: self.script_path(&player.id),
                preprocessed: self.cache_dir.join(format!("{}.pre.js", player.id)),
                sig,
                n,
                reply,
            })
            .map_err(|_| anyhow!("solver thread gone"))?;
        rx.await.context("solver thread dropped job")?
    }

    /// Load the current player into the solver ahead of the first ciphered URL.
    pub async fn warm_up(&self) -> Result<()> {
        let player = self.player().await?;
        self.solve(&player, Vec::new(), Vec::new()).await.map(|_| ())
    }

    /// Old player versions are ~7 MB each (script + preprocessed) and never reused.
    fn remove_other_versions(&self, keep: &str) {
        let Ok(entries) = std::fs::read_dir(&self.cache_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(keep) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Forget the cached player so the next resolve re-checks the version
    /// (after a solve failure the script may have rotated).
    pub async fn invalidate(&self) {
        *self.current.lock().await = None;
    }

    async fn fetch_player_id(&self) -> Result<String> {
        let body = self
            .http
            .get(IFRAME_API_URL)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .context("fetch iframe_api")?
            .text()
            .await?;
        extract_player_id(&body).context("player id not found in iframe_api")
    }

    fn script_path(&self, id: &str) -> PathBuf {
        self.cache_dir.join(format!("{id}.js"))
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `.../s/player/<id>/...` (the iframe API escapes slashes as `\/`).
fn extract_player_id(text: &str) -> Option<String> {
    let unescaped = text.replace("\\/", "/");
    let rest = &unescaped[unescaped.find("/s/player/")? + "/s/player/".len()..];
    let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (id.len() >= 8 && rest[id.len()..].starts_with('/')).then_some(id)
}

fn extract_sts(source: &str) -> Option<u64> {
    for key in ["signatureTimestamp:", "signatureTimestamp=", "sts:"] {
        for (i, _) in source.match_indices(key) {
            let digits: String = source[i + key.len()..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if digits.len() == 5 {
                return digits.parse().ok();
            }
        }
    }
    None
}

/// Owns the QuickJS runtime; keeps the solver functions of one player loaded.
fn solver_thread(rx: mpsc::Receiver<Job>) {
    let mut engine: Option<Engine> = None;
    while let Ok(job) = rx.recv() {
        if engine.as_ref().map(|e| e.player_id != job.player_id).unwrap_or(true) {
            engine = match Engine::load(&job) {
                Ok(e) => Some(e),
                Err(e) => {
                    warn!("loading player {} into solver: {e:#}", job.player_id);
                    let _ = job.reply.send(Err(e));
                    continue;
                }
            };
        }
        let result = engine.as_ref().expect("engine loaded").solve(&job.sig, &job.n);
        let _ = job.reply.send(result);
    }
}

struct Engine {
    player_id: String,
    _runtime: Runtime,
    context: Context,
}

impl Engine {
    fn load(job: &Job) -> Result<Self> {
        let runtime = Runtime::new()?;
        // meriyah recurses deeply over the ~3 MB player AST.
        runtime.set_max_stack_size(64 * 1024 * 1024);
        let context = Context::full(&runtime)?;
        let cached = std::fs::read_to_string(&job.preprocessed).ok();
        context.with(|ctx| -> Result<()> {
            let preprocessed = match cached {
                Some(p) => p,
                None => {
                    let started = Instant::now();
                    ctx.eval::<(), _>(SOLVER_LIB).map_err(|e| js_error(&ctx, e))?;
                    ctx.eval::<(), _>("var meriyah = lib.meriyah; var astring = lib.astring;")
                        .map_err(|e| js_error(&ctx, e))?;
                    ctx.eval::<(), _>(SOLVER_CORE).map_err(|e| js_error(&ctx, e))?;
                    let source = std::fs::read_to_string(&job.script).context("read player script")?;
                    ctx.globals().set("__player", source)?;
                    let pre: String = ctx
                        .eval(
                            r#"(() => {
                                const out = jsc({ type: "player", player: __player,
                                    output_preprocessed: true, requests: [] });
                                delete globalThis.__player;
                                return out.preprocessed_player;
                            })()"#,
                        )
                        .map_err(|e| js_error(&ctx, e))?;
                    info!("preprocessed player {} in {:?}", job.player_id, started.elapsed());
                    if let Err(e) = write_atomic(&job.preprocessed, pre.as_bytes()) {
                        warn!("caching preprocessed player: {e}");
                    }
                    pre
                }
            };
            // Same contract the EJS core uses: the preprocessed script assigns
            // `_result.n` / `_result.sig`, each mapping a challenge to its answer.
            ctx.globals().set("__pre", preprocessed)?;
            ctx.eval::<(), _>(
                r#"globalThis.__solvers = { n: null, sig: null };
                   Function("_result", __pre)(globalThis.__solvers);
                   delete globalThis.__pre;"#,
            )
            .map_err(|e| js_error(&ctx, e))?;
            Ok(())
        })?;
        Ok(Self {
            player_id: job.player_id.clone(),
            _runtime: runtime,
            context,
        })
    }

    fn solve(&self, sig: &[String], n: &[String]) -> Result<Solved> {
        self.context.with(|ctx| {
            let solvers: Object = ctx.globals().get("__solvers")?;
            let mut out = Solved::default();
            for (kind, challenges, dest) in [("sig", sig, &mut out.sig), ("n", n, &mut out.n)] {
                if challenges.is_empty() {
                    continue;
                }
                let Ok(f) = solvers.get::<_, Function>(kind) else {
                    bail!("player has no {kind} function");
                };
                for ch in challenges {
                    let answer: String = f
                        .call((ch.as_str(),))
                        .map_err(|e| js_error(&ctx, e))
                        .with_context(|| format!("solving {kind}"))?;
                    dest.insert(ch.clone(), answer);
                }
            }
            Ok(out)
        })
    }
}

fn js_error(ctx: &rquickjs::Ctx<'_>, err: rquickjs::Error) -> anyhow::Error {
    if matches!(err, rquickjs::Error::Exception) {
        let ex = ctx.catch();
        if let Some(e) = ex.as_exception() {
            return anyhow!("js: {}", e.message().unwrap_or_default());
        }
        if let Some(s) = ex.as_string().and_then(|s| s.to_string().ok()) {
            return anyhow!("js: {s}");
        }
    }
    anyhow!("js: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_player_id_from_iframe_api() {
        let api = r#"var scriptUrl = 'https:\/\/www.youtube.com\/s\/player\/cbcfbf2f\/www-widgetapi.vflset\/www-widgetapi.js';"#;
        assert_eq!(extract_player_id(api).as_deref(), Some("cbcfbf2f"));
        assert_eq!(extract_player_id("nothing here"), None);
    }

    #[test]
    fn parses_signature_timestamp() {
        assert_eq!(extract_sts("a,signatureTimestamp:20746,b"), Some(20746));
        assert_eq!(extract_sts("x={sts:20123}"), Some(20123));
        assert_eq!(extract_sts("signatureTimestamp:1"), None);
    }

    /// Network: fetches the live player and solves a dummy `n` challenge.
    #[tokio::test]
    #[ignore]
    async fn solves_live_player() {
        let dir = std::env::temp_dir().join("yt-tui-jsc-test");
        let jsc = Jsc::new(reqwest::Client::new(), dir);
        let player = jsc.player().await.unwrap();
        let solved = jsc.solve(&player, vec![], vec!["ZdZIqFPQK-Ty8wId".into()]).await.unwrap();
        let answer = &solved.n["ZdZIqFPQK-Ty8wId"];
        assert!(!answer.is_empty() && answer != "ZdZIqFPQK-Ty8wId");
    }
}
