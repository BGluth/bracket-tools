//! The live desk over a start.gg token: look a tournament up, pick its
//! events and stations, preflight, then run the same desk the demo runs,
//! restoring and saving the desk's state through the browser's store.

use std::{
    collections::{BTreeMap, BTreeSet},
    mem,
    rc::Rc,
    str::FromStr,
    time::Duration,
};

use bracket_tools_scheduler_core::{
    app::{seed_from_snapshot, AppState},
    config::{referenced_types, SchedulerConfig, SetupCounts, FALLBACK_SETUPS_PER_TYPE},
    conflict::UnixMillis,
    init::{bracket_config, build_config, parse_tournament_slug, GameSetups, InitError},
    poller::classify_provider_error,
    preflight::{preflight, PreflightEnv, PreflightReport},
    set_source::StartggSource,
    timers::{now_millis, timeout},
};
use bracket_tools_startgg::{types::GGRestToken, EventInfo, GGProvider};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    bridge::{start, Session},
    persist::{age_minutes, apply_saved, peek_saved, DeskPersistence, DeskStore, Load, SavedState},
    views::Desk,
    DESK_CSS,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Store key of the remembered form.
const FORM_KEY: &str = "live-form";

/// The browser store, or why it is unavailable.
type StoreHandle = Result<Rc<DeskStore>, String>;

/// Where the operator is between "no tournament yet" and a running desk.
enum Stage {
    Lookup,
    LookingUp,
    Picking(Tournament),
    Checking,
    Checked(Box<Prepared>),
    Opening,
    Running { session: Session, label: String },
    Failed(String),
}

/// A tournament the token could list, with the source that will poll it.
struct Tournament {
    /// The pinned `tournament/<slug>` form.
    slug: String,
    source: Rc<StartggSource>,
    events: Vec<EventInfo>,
}

/// Preflight's output, waiting for the operator to open the desk.
struct Prepared {
    tournament: Tournament,
    config: SchedulerConfig,
    report: PreflightReport,
    persistence: Option<Rc<DeskPersistence>>,
    saved: SavedState,
}

/// What the operator entered last time, so a reload lands on the same picks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LiveForm {
    tournament: String,
    /// The pinned slug the picks below belong to.
    slug: String,
    selected: Vec<String>,
    counts: BTreeMap<String, String>,
    arm_writes: bool,
}

/// The scheduler run live against start.gg. `token` is the site's start.gg
/// token; `None` asks for one.
#[component]
pub fn SchedulerTool(token: ReadSignal<Option<String>>) -> Element {
    let mut stage = use_signal(|| Stage::Lookup);
    let mut tournament_input = use_signal(String::new);
    let mut selected = use_signal(BTreeSet::<String>::new);
    let mut counts = use_signal(BTreeMap::<String, String>::new);
    let mut arm_writes = use_signal(|| false);
    let mut notice = use_signal(|| None::<String>);
    let mut remembered = use_signal(|| None::<LiveForm>);
    let store = use_resource(move || async move {
        let store = DeskStore::open().await?;
        if let Some(form) = store.load::<LiveForm>(FORM_KEY).await? {
            tournament_input.set(form.tournament.clone());
            remembered.set(Some(form));
        }
        Ok::<Rc<DeskStore>, String>(store)
    });
    let store_handle: StoreHandle = store
        .read()
        .clone()
        .unwrap_or_else(|| Err("browser storage is still opening".to_owned()));

    let Some(raw_token) = token.read().clone() else {
        return rsx! {
            document::Link { rel: "stylesheet", href: DESK_CSS }
            div { class: "empty", "The live desk needs a start.gg token. Add one in Settings." }
        };
    };

    let on_look_up = move |_: MouseEvent| {
        let raw_token = raw_token.clone();
        let input = tournament_input();
        stage.set(Stage::LookingUp);
        spawn(async move {
            match look_up(raw_token, input).await {
                Ok(tournament) => {
                    let form = remembered.read().clone().filter(|form| form.slug == tournament.slug);
                    match form {
                        Some(form) => {
                            selected.set(form.selected.into_iter().collect());
                            counts.set(form.counts);
                            arm_writes.set(form.arm_writes);
                        }
                        None => {
                            selected.set(tournament.events.iter().map(|event| event.slug.clone()).collect());
                            counts.set(BTreeMap::new());
                        }
                    }
                    notice.set(None);
                    stage.set(Stage::Picking(tournament));
                }
                Err(error) => stage.set(Stage::Failed(error)),
            }
        });
    };
    let check_store = store_handle.clone();
    let on_check = move |_: MouseEvent| {
        let previous = mem::replace(&mut *stage.write(), Stage::Checking);
        let Stage::Picking(tournament) = previous else {
            stage.set(previous);
            return;
        };
        match live_config(&tournament, &selected.read(), &counts.read()) {
            Ok(config) => {
                notice.set(None);
                let arm = arm_writes();
                let form = LiveForm {
                    tournament: tournament_input(),
                    slug: tournament.slug.clone(),
                    selected: selected.read().iter().cloned().collect(),
                    counts: counts(),
                    arm_writes: arm,
                };
                let store = check_store.clone();
                spawn(async move {
                    let (persistence, saved) = match &store {
                        Ok(store) => {
                            // Best effort: a failed form save shows up as a
                            // storage problem on the report, not here.
                            let _ = store.save(FORM_KEY, &form).await;
                            let persistence = Rc::new(DeskPersistence::new(store.clone(), bare_slug(&tournament.slug)));
                            let saved = peek_saved(&persistence).await;
                            (Some(persistence), saved)
                        }
                        Err(error) => (None, SavedState::Unavailable(error.clone())),
                    };
                    let env = PreflightEnv::silent();
                    let report = preflight(&*tournament.source, &config, REQUEST_TIMEOUT, arm, classify_provider_error, &env).await;
                    stage.set(Stage::Checked(Box::new(Prepared {
                        tournament,
                        config,
                        report,
                        persistence,
                        saved,
                    })));
                });
            }
            Err(error) => {
                notice.set(Some(error));
                stage.set(Stage::Picking(tournament));
            }
        }
    };
    let on_open = move |_: MouseEvent| {
        let previous = mem::replace(&mut *stage.write(), Stage::Opening);
        let Stage::Checked(prepared) = previous else {
            stage.set(previous);
            return;
        };
        spawn(async move {
            let (session, label) = open_desk(*prepared).await;
            stage.set(Stage::Running { session, label });
        });
    };
    let on_discard = move |_: MouseEvent| {
        let persistence = match &*stage.read() {
            Stage::Checked(prepared) => prepared.persistence.clone(),
            _ => None,
        };
        let Some(persistence) = persistence else { return };
        spawn(async move {
            let saved = match persistence.forget().await {
                Ok(()) => SavedState::None,
                Err(error) => SavedState::Unavailable(error),
            };
            if let Stage::Checked(prepared) = &mut *stage.write() {
                prepared.saved = saved;
            }
        });
    };
    let on_back_to_picker = move |_: MouseEvent| {
        let previous = mem::replace(&mut *stage.write(), Stage::Lookup);
        match previous {
            Stage::Checked(prepared) => stage.set(Stage::Picking(prepared.tournament)),
            other => stage.set(other),
        }
    };
    let on_back_to_lookup = move |_: MouseEvent| stage.set(Stage::Lookup);

    let view = match &*stage.read() {
        Stage::Lookup => lookup_view(tournament_input, on_look_up),
        Stage::LookingUp => status_view(format!("looking up {}…", tournament_input.read().trim())),
        Stage::Picking(tournament) => picker_view(tournament, selected, counts, arm_writes, notice, on_check, on_back_to_lookup),
        Stage::Checking => status_view("preflighting the chosen events…".to_owned()),
        Stage::Checked(prepared) => report_view(prepared, on_open, on_discard, on_back_to_picker),
        Stage::Opening => status_view("opening the desk…".to_owned()),
        Stage::Running { session, label } => rsx! { Desk { session: session.clone(), mode: label.clone() } },
        Stage::Failed(error) => failed_view(error, on_back_to_lookup),
    };
    rsx! {
        document::Link { rel: "stylesheet", href: DESK_CSS }
        {view}
    }
}

async fn look_up(raw_token: String, input: String) -> Result<Tournament, String> {
    let token = GGRestToken::from_str(raw_token.trim()).map_err(|e| e.to_string())?;
    let slug = parse_tournament_slug(&input).map_err(|e| e.to_string())?;
    let provider = GGProvider::builder(token).build().map_err(|e| e.to_string())?;
    let events = timeout(REQUEST_TIMEOUT, provider.fetch_tournament_events(&slug))
        .await
        .map_err(|_| format!("start.gg did not answer within {}s", REQUEST_TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())?;
    if events.is_empty() {
        return Err(InitError::NoEvents { slug }.to_string());
    }
    Ok(Tournament {
        slug,
        source: Rc::new(StartggSource::new(provider)),
        events,
    })
}

/// Bootstraps the desk the way the TUI launches: fetch-failed events seed
/// from the saved snapshot, the saved overlay rehydrates over the fresh
/// state (its board wins), then the loops start.
async fn open_desk(prepared: Prepared) -> (Session, String) {
    let Prepared {
        tournament,
        mut config,
        report,
        persistence,
        saved,
    } = prepared;
    if report.escalate_soft_busy {
        config.escalate_unpinned_state_deviation = true;
    }
    let writes_armed = report.writes_armed;
    let label = bare_slug(&tournament.slug).to_owned();
    let mut bootstraps = report.into_bootstraps();
    let now = now_millis();
    let seeded = match &persistence {
        Some(persistence) => match persistence.load_snapshot().await {
            Ok(Load::Loaded(saved)) => seed_from_snapshot(&mut bootstraps, &saved.doc),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    let mut state = AppState::new(config, writes_armed, bootstraps, now);
    apply_saved(&mut state, saved, now);
    state.mark_seeded_stale(&seeded, now);
    (start(tournament.source, state, classify_provider_error, persistence), label)
}

/// The in-memory config for the chosen events: the seeded brackets plus the
/// operator's station counts (blank = the fallback count).
fn live_config(tournament: &Tournament, selected: &BTreeSet<String>, counts: &BTreeMap<String, String>) -> Result<SchedulerConfig, String> {
    let chosen = chosen_events(tournament, selected);
    let mut config = build_config(&tournament.slug, &chosen, &GameSetups::default());
    let mut table = BTreeMap::new();
    for setup_type in referenced_types(&config) {
        let count = match counts.get(&setup_type).map(|raw| raw.trim()).filter(|raw| !raw.is_empty()) {
            None => FALLBACK_SETUPS_PER_TYPE,
            Some(raw) => raw.parse().map_err(|_| format!("{setup_type}: {raw:?} is not a station count"))?,
        };
        table.insert(setup_type, count);
    }
    config.setups = Some(SetupCounts::ByType(table));
    config.validate().map_err(|e| e.to_string())?;
    Ok(config)
}

fn chosen_events(tournament: &Tournament, selected: &BTreeSet<String>) -> Vec<EventInfo> {
    tournament
        .events
        .iter()
        .filter(|event| selected.contains(&event.slug))
        .cloned()
        .collect()
}

fn bare_slug(slug: &str) -> &str {
    slug.strip_prefix("tournament/").unwrap_or(slug)
}

fn seeded_types_text(event: &EventInfo) -> String {
    bracket_config(event, &GameSetups::default()).setup_types().join(" ")
}

fn lookup_view(mut input: Signal<String>, on_look_up: impl FnMut(MouseEvent) + 'static) -> Element {
    let empty = input.read().trim().is_empty();
    rsx! {
        section { class: "live",
            h2 { "Live desk" }
            div { class: "controls",
                input {
                    r#type: "text",
                    placeholder: "start.gg tournament URL or slug",
                    value: "{input}",
                    oninput: move |event| input.set(event.value()),
                }
                button { disabled: empty, onclick: on_look_up, "look up" }
            }
            p { class: "dim", "The token stays in this browser; start.gg is called directly from it." }
        }
    }
}

fn picker_view(
    tournament: &Tournament,
    mut selected: Signal<BTreeSet<String>>,
    mut counts: Signal<BTreeMap<String, String>>,
    mut arm_writes: Signal<bool>,
    notice: Signal<Option<String>>,
    on_check: impl FnMut(MouseEvent) + 'static,
    on_back: impl FnMut(MouseEvent) + 'static,
) -> Element {
    let chosen = chosen_events(tournament, &selected.read());
    let types = referenced_types(&build_config(&tournament.slug, &chosen, &GameSetups::default()));
    let name = bare_slug(&tournament.slug);
    let total = tournament.events.len();
    let rows = tournament.events.iter().map(|event| {
        let slug = event.slug.clone();
        let on = selected.read().contains(&slug);
        rsx! {
            label { class: "event",
                input {
                    r#type: "checkbox",
                    checked: on,
                    onchange: move |event| {
                        let on = event.checked();
                        selected.with_mut(|chosen| {
                            if on {
                                chosen.insert(slug.clone());
                            } else {
                                chosen.remove(&slug);
                            }
                        });
                    },
                }
                span { class: "name", {event.name.clone().unwrap_or_else(|| event.slug.clone())} }
                span { class: "game", {event.videogame.clone().unwrap_or_default()} }
                span { class: "types", {seeded_types_text(event)} }
            }
        }
    });
    let count_fields = types.iter().map(|setup_type| {
        let key = setup_type.clone();
        let value = counts
            .read()
            .get(setup_type)
            .cloned()
            .unwrap_or_else(|| FALLBACK_SETUPS_PER_TYPE.to_string());
        rsx! {
            label {
                {setup_type.clone()}
                input {
                    r#type: "text",
                    value,
                    oninput: move |event| {
                        counts.with_mut(|table| {
                            table.insert(key.clone(), event.value());
                        });
                    },
                }
            }
        }
    });
    rsx! {
        section { class: "live",
            h2 { "{name}: {total} events" }
            div { class: "events", {rows} }
            h2 { "Stations per setup type" }
            div { class: "counts", {count_fields} }
            label { class: "controls",
                input { r#type: "checkbox", checked: arm_writes(), onchange: move |event| arm_writes.set(event.checked()) }
                "arm writes: calls and reports reach start.gg (needs an admin token; preflight decides)"
            }
            if let Some(text) = notice.read().as_ref() {
                p { class: "failed", {text.clone()} }
            }
            div { class: "controls",
                button { disabled: chosen.is_empty(), onclick: on_check, "preflight" }
                button { class: "quiet", onclick: on_back, "back" }
            }
        }
    }
}

fn report_view(
    prepared: &Prepared,
    on_open: impl FnMut(MouseEvent) + 'static,
    on_discard: impl FnMut(MouseEvent) + 'static,
    on_back: impl FnMut(MouseEvent) + 'static,
) -> Element {
    let blocked = prepared.report.fatal.is_some();
    let saved = saved_line(&prepared.saved, now_millis());
    rsx! {
        section { class: "live",
            h2 { "Preflight" }
            pre { class: "report", {prepared.report.render()} }
            if let Some((text, restorable)) = saved {
                div { class: "controls",
                    span { class: "dim", {text} }
                    if restorable {
                        button { class: "quiet", onclick: on_discard, "discard saved state" }
                    }
                }
            }
            div { class: "controls",
                button { disabled: blocked, onclick: on_open, "open desk" }
                button { class: "quiet", onclick: on_back, "back" }
                if blocked {
                    span { class: "dim", "preflight failed; adjust the picks and try again" }
                }
            }
        }
    }
}

/// The saved-state line under the report, and whether "discard" applies.
fn saved_line(saved: &SavedState, now: UnixMillis) -> Option<(String, bool)> {
    match saved {
        SavedState::None => None,
        SavedState::Found(saved) => Some((
            format!(
                "desk state saved {}m ago will be restored (its stations win)",
                age_minutes(saved.saved_at, now)
            ),
            true,
        )),
        SavedState::Corrupt => Some((
            "saved desk state was corrupt or from another version; it was set aside".to_owned(),
            false,
        )),
        SavedState::Unavailable(error) => Some((format!("browser storage unavailable; nothing will be saved ({error})"), false)),
    }
}

fn status_view(text: String) -> Element {
    rsx! { div { class: "loading", {text} } }
}

fn failed_view(error: &str, on_back: impl FnMut(MouseEvent) + 'static) -> Element {
    rsx! {
        section { class: "live",
            div { class: "failed", {error.to_owned()} }
            div { class: "controls",
                button { class: "quiet", onclick: on_back, "back" }
            }
        }
    }
}
