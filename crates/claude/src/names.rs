//! Session names (giverny#343): with Settings → Management panel → "name
//! every session" on, every Claude session in a Giverny tab is
//! named at its start, before its first turn (giverny#401): its
//! `SessionStart` hook draws it a first name ([`draw_name`], giverny#364:
//! sessions left to pick one kept picking the same few), gives it the
//! surname of where it was born ([`surname`], giverny#434), records it, and
//! tells the session. The status line shows it beside the model from the
//! start: `Opus 5.5 (Rotem Giverny)`.
//!
//! **The surname is the session's category** (giverny#434), as surnames
//! once were: the rail category its tab sits in when it is named, as Ita
//! wrote it with the first letter capitalised (`giverny` → `Giverny`,
//! `ToLaD` stays), else, in no category, its repo. Not drawn. It is fixed
//! at birth like a real one: recorded with the first name and never
//! recomputed, so moving or renaming the tab renames nobody, and a resumed
//! session keeps the name it had, an older drawn one (`Rotem Ashby`) too.
//!
//! A name is given once: `giverny-manage name` prints the one recorded. A
//! session may still give its own: a first name alone takes its category
//! surname, a full name passed in (a revived session keeping its old one) is
//! kept as given, and naming again with the same first name keeps the
//! surname already recorded, so nobody is renamed halfway through.
//!
//! **No two live sessions share a first name** (giverny#425): Ita tells his
//! sessions apart by it, two managers in one category above all. A session
//! is live while its claude runs, by Claude Code's own registry
//! ([`live_sessions`]), so a first name comes free again once its session
//! ends and the pool never fills up over months. A drawn first name is
//! drawn from the pool less the ones live sessions hold ([`draw_name`]); one
//! given by hand that another live session holds is refused, naming the
//! holder ([`record`]); and a resumed session takes its conversation's old
//! name back only while nobody live took that first name meanwhile
//! ([`find`]), else it is drawn a new one. The same session, or another id
//! of the same conversation, re-taking its own name is fine. A full name
//! need not be unique over time: a second Rotem Giverny once the first has
//! ended is fine.
//!
//! **Where a name lives.** Beside the feeds, never in one: a session that
//! only names itself is no manager, and a feed file would make it one.
//! `<feeds>/names/<session>.json` holds `{session, name, room, root}` (the
//! room its tab can be titled after, [`ROOMS`], giverny#435), and
//! `<feeds>/names/roots/<root>.json` the same, keyed by the conversation's
//! root ([`crate::continuation`]): a resumed session has a new id but the same
//! root, so the status line finds its name there and copies it under the new
//! id (giverny#336). A session named at its start has no root yet (its
//! transcript holds no turn), so its first prompt and the status line stamp
//! one in as soon as the transcript has it ([`keep_root`]). `<feeds>/names/on`
//! exists while the switch is on; the app writes and removes it, and the
//! status line and the session-start hook read it.
//!
//! **When a session is named** (giverny#356). Claude Code reads a plugin's
//! `hooks.json` once, when its process starts (and on `/reload-plugins`),
//! never on `/clear`, a compaction or a resume, measured on 2.1.296: a claude
//! started before the plugin gained a hook never runs it. So the plugin's
//! `SessionStart` hook is always registered and reads the switch itself
//! ([`session_start`]), and flipping it reaches every session started since
//! at its next start. A session whose process predates the hook, or that
//! started with the switch off, is named on its next prompt instead, by the
//! `UserPromptSubmit` hook it does run ([`first_prompt`]). Either way a
//! session is named and told once: `<feeds>/names/asked/<session>` marks the
//! ones told.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The names directory under the feed directory.
pub const DIR: &str = "names";
/// The switch, as a file in [`DIR`]: present while sessions are named and
/// their names shown.
const ON: &str = "on";
/// Under [`DIR`], an empty file per session already told its name
/// ([`tell`]), so the prompt hook's fallback ([`first_prompt`]) never tells
/// it twice.
const ASKED: &str = "asked";
/// Under [`DIR`], the lock every rewrite of a names document is made under
/// ([`lock`], giverny#433, giverny#441), so two sessions drawing at once
/// never share a first name and a room is drawn once.
const LOCK: &str = ".lock";
/// How long a names document just written counts its session as live
/// though Claude Code's registry does not list it yet (giverny#433): a
/// session drawing in the instant another starts sees its name.
const FRESH: std::time::Duration = std::time::Duration::from_secs(30);

/// Giverny's own first names, drawn for a session that asks for a name
/// without giving one (giverny#364): sessions left to pick kept picking the
/// same few. Personal names of many origins, short and easy to say; none a
/// famous single-name figure, a tool or language (`Ruby`, `Zig`), or a word
/// that would read as part of a log line (`May`, `Max`, `Dev`).
pub const FIRST_NAMES: &[&str] = &[
    "Aarav",
    "Abel",
    "Abena",
    "Adaeze",
    "Ade",
    "Adi",
    "Aditi",
    "Agata",
    "Agnes",
    "Aiko",
    "Akira",
    "Alba",
    "Alden",
    "Alexis",
    "Ama",
    "Amaia",
    "Amara",
    "Amir",
    "Amit",
    "Anh",
    "Anika",
    "Anjali",
    "Anke",
    "Ansel",
    "Anya",
    "Arda",
    "Arjun",
    "Arlo",
    "Arne",
    "Arthur",
    "Asha",
    "Astrid",
    "Audrey",
    "Ayla",
    "Ayo",
    "Bahar",
    "Bao",
    "Barnaby",
    "Bastian",
    "Beatrix",
    "Beau",
    "Benicio",
    "Bisi",
    "Bjorn",
    "Bogdan",
    "Borys",
    "Bram",
    "Briony",
    "Britt",
    "Bruno",
    "Calla",
    "Callum",
    "Camila",
    "Cassia",
    "Catalina",
    "Cecily",
    "Cem",
    "Cerys",
    "Chiamaka",
    "Chidi",
    "Ciaran",
    "Clara",
    "Cole",
    "Cormac",
    "Cosima",
    "Cyrus",
    "Dafna",
    "Dagny",
    "Daiki",
    "Dalia",
    "Damon",
    "Danica",
    "Daphne",
    "Darina",
    "Dario",
    "Dariush",
    "Darya",
    "Dayo",
    "Declan",
    "Defne",
    "Delphine",
    "Diego",
    "Dimitra",
    "Dirk",
    "Dmitri",
    "Dorothea",
    "Dragan",
    "Eamon",
    "Ebba",
    "Edda",
    "Edith",
    "Edwin",
    "Eitan",
    "Ekene",
    "Elena",
    "Eleni",
    "Eli",
    "Elif",
    "Elin",
    "Elio",
    "Eliza",
    "Elke",
    "Ellis",
    "Elsie",
    "Elspeth",
    "Emi",
    "Emilio",
    "Emre",
    "Emrys",
    "Enzo",
    "Esme",
    "Esteban",
    "Etta",
    "Ewan",
    "Ezra",
    "Fabio",
    "Farah",
    "Felix",
    "Femi",
    "Femke",
    "Fenna",
    "Fergus",
    "Finn",
    "Fiona",
    "Flavia",
    "Folake",
    "Freya",
    "Frida",
    "Funmi",
    "Gael",
    "Gali",
    "Gareth",
    "Gideon",
    "Greta",
    "Gunnar",
    "Gus",
    "Gwen",
    "Hana",
    "Hanne",
    "Harriet",
    "Haruto",
    "Hattie",
    "Hedda",
    "Henrik",
    "Hester",
    "Hila",
    "Hiro",
    "Hoa",
    "Hollis",
    "Hyun",
    "Ido",
    "Idris",
    "Ignacio",
    "Ilias",
    "Ilse",
    "Ilya",
    "Imani",
    "Imelda",
    "Imogen",
    "Ines",
    "Ingo",
    "Ingrid",
    "Ione",
    "Irina",
    "Iris",
    "Ishaan",
    "Ishani",
    "Isla",
    "Ivana",
    "Ivo",
    "Ivor",
    "Jabari",
    "Jana",
    "Jarek",
    "Jasper",
    "Jens",
    "Jia",
    "Jiwoo",
    "Joaquin",
    "Jonah",
    "Jonas",
    "Joost",
    "Jude",
    "Julieta",
    "Juniper",
    "Kagiso",
    "Kai",
    "Kaito",
    "Kaja",
    "Kasia",
    "Kasimir",
    "Katya",
    "Kavya",
    "Keiko",
    "Keir",
    "Kenji",
    "Kerem",
    "Kian",
    "Kiran",
    "Kjell",
    "Klara",
    "Kofi",
    "Kojo",
    "Kostas",
    "Kwame",
    "Kwesi",
    "Lachlan",
    "Lars",
    "Layla",
    "Leandro",
    "Leda",
    "Leela",
    "Leif",
    "Lena",
    "Leo",
    "Lerato",
    "Lev",
    "Liesel",
    "Lindiwe",
    "Linh",
    "Linnea",
    "Lionel",
    "Lior",
    "Liora",
    "Lorcan",
    "Lorena",
    "Lorna",
    "Lottie",
    "Luca",
    "Lucia",
    "Luka",
    "Mabel",
    "Maeve",
    "Magnus",
    "Mairi",
    "Makena",
    "Malik",
    "Marek",
    "Maren",
    "Marisol",
    "Marit",
    "Marlowe",
    "Mateo",
    "Matteo",
    "Mavis",
    "Maya",
    "Meera",
    "Mei",
    "Mila",
    "Milena",
    "Milo",
    "Mina",
    "Minjun",
    "Mio",
    "Mirabel",
    "Mirek",
    "Nadia",
    "Nalini",
    "Nao",
    "Naoko",
    "Nasrin",
    "Nathaniel",
    "Navid",
    "Ned",
    "Neel",
    "Nell",
    "Neta",
    "Nia",
    "Nico",
    "Nikhil",
    "Nikos",
    "Nils",
    "Nina",
    "Nkiru",
    "Noa",
    "Noam",
    "Nolan",
    "Nora",
    "Obi",
    "Odile",
    "Oksana",
    "Oliver",
    "Omar",
    "Omri",
    "Oona",
    "Oren",
    "Orla",
    "Oskar",
    "Otis",
    "Ottilie",
    "Otto",
    "Paloma",
    "Parisa",
    "Pavel",
    "Percy",
    "Petra",
    "Phoebe",
    "Pieter",
    "Pilar",
    "Pippa",
    "Priya",
    "Quan",
    "Quentin",
    "Quinn",
    "Radek",
    "Rahul",
    "Ralf",
    "Rami",
    "Ramona",
    "Ravi",
    "Renata",
    "Reuben",
    "Reza",
    "Rhys",
    "Riku",
    "Rin",
    "Rocio",
    "Rodrigo",
    "Rohan",
    "Ronan",
    "Roni",
    "Rosa",
    "Rosalind",
    "Rotem",
    "Rowan",
    "Rufus",
    "Runa",
    "Ruth",
    "Sadie",
    "Saki",
    "Sameer",
    "Samira",
    "Sanjay",
    "Sanne",
    "Sasha",
    "Selim",
    "Seren",
    "Seung",
    "Shira",
    "Shirin",
    "Sigrid",
    "Silas",
    "Simeon",
    "Sipho",
    "Solveig",
    "Soraya",
    "Stavros",
    "Stefan",
    "Stellan",
    "Sunil",
    "Sven",
    "Sybil",
    "Tabitha",
    "Takumi",
    "Tal",
    "Tamar",
    "Tamsin",
    "Tara",
    "Tarek",
    "Taro",
    "Tegan",
    "Tendai",
    "Teodor",
    "Tess",
    "Thalia",
    "Thandi",
    "Thao",
    "Thea",
    "Themba",
    "Theo",
    "Tiago",
    "Tobias",
    "Tobin",
    "Toby",
    "Tomasz",
    "Tove",
    "Tuan",
    "Tunde",
    "Ulla",
    "Una",
    "Ursula",
    "Valentin",
    "Valeria",
    "Varun",
    "Vera",
    "Vernon",
    "Vesna",
    "Vidar",
    "Vikram",
    "Walt",
    "Wanjiru",
    "Wei",
    "Wes",
    "Wilfred",
    "Willa",
    "Wim",
    "Winifred",
    "Wren",
    "Xanthe",
    "Ximena",
    "Xin",
    "Yael",
    "Yannick",
    "Yannis",
    "Yara",
    "Ylva",
    "Yuki",
    "Yuna",
    "Yuri",
    "Yusuf",
    "Yuto",
    "Zaira",
    "Zara",
    "Zawadi",
    "Zeke",
    "Zeynep",
    "Ziv",
    "Zoe",
    "Zofia",
    "Zola",
    "Zoran",
    "Zuri",
];

/// The rooms a session's tab can be titled after (giverny#435): with
/// Settings → tabs & titles → "name Claude tabs after their session" on, a
/// named session's tab reads as its place, `Rotem's Workshop`. One is drawn
/// per session at its birth and recorded beside its name ([`room`]), so the
/// title holds across refreshes and resumes. Rooms someone works in, one
/// word, short enough for the rail.
pub const ROOMS: &[&str] = &[
    "Atelier",
    "Attic",
    "Cabin",
    "Cellar",
    "Conservatory",
    "Den",
    "Forge",
    "Gallery",
    "Garden",
    "Greenhouse",
    "Lab",
    "Library",
    "Lighthouse",
    "Loft",
    "Nook",
    "Observatory",
    "Office",
    "Parlour",
    "Porch",
    "Shed",
    "Studio",
    "Study",
    "Tower",
    "Workshop",
];

/// A room from [`ROOMS`], at random.
pub fn draw_room() -> String {
    pick(ROOMS)
}

/// `name` at home in `room`: `Rotem Giverny` in the Workshop is
/// `Rotem's Workshop`. The first name alone, as Ita calls his sessions.
pub fn place_of(name: &str, room: &str) -> Option<String> {
    let first = name.split_whitespace().next()?;
    let room = room.trim();
    (!room.is_empty()).then(|| format!("{first}'s {room}"))
}

/// The environment variable a Giverny tab's shell carries the name of the
/// rail category the tab sits in when its shell starts (giverny#434): what a
/// session born there takes as its surname ([`surname`]) when the app's
/// saved workspace cannot say where the tab sits now (giverny#439).
pub const CATEGORY_ENV: &str = "GIVERNY_CATEGORY";

/// The surname a session is born with (giverny#434): the name of the rail
/// category its tab sits in, as Ita wrote it, first letter capitalised
/// (`giverny` → `Giverny`, `ToLaD` stays); with no category, the repo it
/// runs in (`cwd`'s git root, a worktree counting as its main checkout,
/// else `cwd`'s own name). Not drawn: like a real surname it says where a
/// session comes from, and it is fixed at birth: recorded with the first
/// name, it is never recomputed, so moving or renaming the tab later
/// renames nobody. `feeds` is the app's feed directory, which finds its
/// `state/tabs.json`: the tab's category there comes first (giverny#439),
/// [`CATEGORY_ENV`] only when the tab is not found in it.
pub fn surname(feeds: &Path, cwd: Option<&Path>) -> String {
    surname_in(&|name| std::env::var(name).ok(), feeds, cwd)
}

/// [`surname`], reading the environment through `get`.
pub fn surname_in(
    get: &dyn Fn(&str) -> Option<String>,
    feeds: &Path,
    cwd: Option<&Path>,
) -> String {
    let var = |name: &str| crate::lineage::giverny_var_in(name, get);
    // The category the tab sits in now, by the workspace the app saves
    // within seconds of a change (giverny#439): a tab moved, or its
    // category renamed, since its shell started. The shell's own word for
    // it only where the app's state is out of reach (a WSL tab of a Windows
    // Giverny) or does not know the tab.
    tabs_file(feeds)
        .zip(var("GIVERNY_TAB_ID"))
        .and_then(|(tabs, tab)| tab_category(&tabs, &tab))
        .or_else(|| var(CATEGORY_ENV))
        .and_then(|c| as_surname(&c))
        .or_else(|| {
            let cwd = cwd
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())?;
            as_surname(&crate::manage_history::repo_of("", &cwd)?)
        })
        .unwrap_or_else(|| "Giverny".to_string())
}

/// The app's saved workspace beside its feed directory
/// (`<base>/feeds` → `<base>/state/tabs.json`); none for a feed directory
/// that is not one of the app's own.
fn tabs_file(feeds: &Path) -> Option<PathBuf> {
    if feeds.file_name()? != "feeds" {
        return None;
    }
    Some(feeds.parent()?.join("state").join("tabs.json"))
}

/// The name of the category tab `tab` (`giverny-<n>`, as `GIVERNY_TAB_ID`
/// carries it) sits in, by the workspace saved at `tabs`. Read only: the
/// app owns the file.
fn tab_category(tabs: &Path, tab: &str) -> Option<String> {
    let n: u64 = tab.trim().strip_prefix("giverny-")?.parse().ok()?;
    let doc: Value = serde_json::from_slice(&std::fs::read(tabs).ok()?).ok()?;
    let ws = doc.get("workspace")?;
    let category = ws
        .get("tabs")?
        .as_array()?
        .iter()
        .find(|t| t.get("id").and_then(Value::as_u64) == Some(n))?
        .get("category")?
        .as_u64()?;
    ws.get("categories")?
        .as_array()?
        .iter()
        .find(|c| c.get("id").and_then(Value::as_u64) == Some(category))?
        .get("name")?
        .as_str()
        .map(String::from)
}

/// `name` as a surname: one word of a name ([`is_word`]), kept readable
/// rather than refused. Letters, digits and apostrophes stay; spaces and any
/// other character become one hyphen (`lead machine` → `Lead-machine`);
/// what does not start with a letter loses its lead (`3D prints` →
/// `D-prints`); the first letter is capitalised, the rest kept as written.
/// `None` when no letter is left.
pub fn as_surname(name: &str) -> Option<String> {
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = if c.is_alphanumeric() || c == '\'' {
            c
        } else {
            '-'
        };
        if out.is_empty() && !c.is_alphabetic() {
            continue;
        }
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    let mut out: String = out.chars().take(40).collect();
    while out.ends_with(['-', '\'']) {
        out.pop();
    }
    let mut chars = out.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// `<feeds>/names`.
pub fn dir(feeds: &Path) -> PathBuf {
    feeds.join(DIR)
}

/// Are sessions named, and their names shown? A `stat`.
pub fn is_on(feeds: &Path) -> bool {
    dir(feeds).join(ON).is_file()
}

/// Follow the switch: write or remove `<feeds>/names/on`. Returns whether
/// anything changed.
pub fn set_on(feeds: &Path, on: bool) -> std::io::Result<bool> {
    let flag = dir(feeds).join(ON);
    if on == flag.is_file() {
        return Ok(false);
    }
    if on {
        std::fs::create_dir_all(dir(feeds))?;
        std::fs::write(&flag, "")?;
    } else {
        std::fs::remove_file(&flag)?;
    }
    Ok(true)
}

/// An id usable as a file name.
fn safe(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\']) && !id.starts_with('.')
}

fn session_file(feeds: &Path, session: &str) -> Option<PathBuf> {
    safe(session).then(|| dir(feeds).join(format!("{session}.json")))
}

fn asked_file(feeds: &Path, session: &str) -> Option<PathBuf> {
    safe(session).then(|| dir(feeds).join(ASKED).join(session))
}

fn root_file(feeds: &Path, root: &str) -> Option<PathBuf> {
    safe(root).then(|| dir(feeds).join("roots").join(format!("{root}.json")))
}

/// The `name` a names document at `file` holds.
fn name_in(file: &Path) -> Option<String> {
    doc_name(&read_doc(file)?)
}

/// A names document, parsed.
fn read_doc(file: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

/// The `name` a names document holds.
fn doc_name(doc: &Value) -> Option<String> {
    doc.get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(String::from)
}

/// The `room` a names document holds (giverny#435).
fn doc_room(doc: &Value) -> Option<String> {
    doc.get("room")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(String::from)
}

fn write_doc(file: &Path, doc: &Value) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{doc}\n"))?;
    std::fs::rename(&tmp, file)
}

/// The names lock (giverny#433, giverny#441): held, every read of what
/// others hold and every rewrite of a names document is one step, so two
/// sessions starting at once draw in turn, and a room is drawn and written
/// once. Released when dropped. Best effort: where the file system takes
/// no lock (a share), the step runs unlocked, as it always did.
#[must_use]
struct Lock(#[allow(dead_code)] Option<std::fs::File>);

fn lock(feeds: &Path) -> Lock {
    let _ = std::fs::create_dir_all(dir(feeds));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir(feeds).join(LOCK))
        .ok()
        .filter(|f| f.lock().is_ok());
    Lock(file)
}

/// The name recorded for `session` itself: one small file read.
pub fn read(feeds: &Path, session: &str) -> Option<String> {
    name_in(&session_file(feeds, session)?)
}

/// `session`'s name: its own, else the one recorded for the conversation its
/// transcript holds (a resumed session's new id, giverny#336), which is then
/// recorded under the new id too, so the next look is one read again. Not
/// the conversation's name when another live session took its first name
/// while this one was gone (giverny#425): then none, and the session is
/// drawn a new one where it is named.
pub fn find(feeds: &Path, session: &str, transcript: Option<&Path>) -> Option<String> {
    find_among(feeds, session, transcript, || live_sessions(None))
}

/// [`find`], asking `live` for the live sessions only when it takes a
/// conversation's name.
pub fn find_among(
    feeds: &Path,
    session: &str,
    transcript: Option<&Path>,
    live: impl FnOnce() -> Live,
) -> Option<String> {
    if let Some(name) = read(feeds, session) {
        return Some(name);
    }
    let root = crate::subagents::conversation_root(transcript?)?;
    let doc = read_doc(&root_file(feeds, &root)?)?;
    let name = doc_name(&doc)?;
    let live = live();
    // Checked and taken in one step (giverny#433).
    let _lock = lock(feeds);
    if let Some(name) = read(feeds, session) {
        return Some(name);
    }
    if held_by(&holders(feeds, &live, session, Some(&root)), &name).is_some() {
        return None;
    }
    if let Some(file) = session_file(feeds, session) {
        let mut copy = json!({"session": session, "name": name, "root": root});
        // Its room comes with it (giverny#435): the same conversation, the
        // same place.
        if let Some(room) = doc_room(&doc) {
            copy["room"] = json!(room);
        }
        let _ = write_doc(&file, &copy);
    }
    Some(name)
}

/// The `name` on `session`'s manager feed: one named before names had their
/// own home (giverny#330). One read, no search.
fn on_feed(feeds: &Path, session: &str) -> Option<String> {
    crate::feed::read(&crate::feed::feed_path(feeds, session))?
        .name
        .filter(|n| !n.trim().is_empty())
}

/// Stamp `session`'s conversation root into its name, once its `transcript`
/// has one (giverny#401): a session named at its start has no turn yet, so
/// no root, and without one a resume (a new id, the same conversation)
/// would not find the name ([`find`]). Writes the root's file too. Returns
/// the name recorded, if any; a session already stamped costs one read.
pub fn keep_root(feeds: &Path, session: &str, transcript: Option<&Path>) -> Option<String> {
    let file = session_file(feeds, session)?;
    let doc = read_doc(&file)?;
    let name = doc_name(&doc)?;
    let unstamped = |doc: &Value| doc.get("root").and_then(Value::as_str).is_none();
    if unstamped(&doc)
        && let Some(root) = transcript.and_then(crate::subagents::conversation_root)
    {
        // Re-read under the lock: a room drawn meanwhile ([`room`]) stays
        // (giverny#441).
        let _lock = lock(feeds);
        let Some(mut doc) = read_doc(&file).filter(unstamped) else {
            return Some(name);
        };
        doc["root"] = json!(root);
        let _ = write_doc(&file, &doc);
        if let Some(rf) = root_file(feeds, &root) {
            let _ = write_doc(&rf, &doc);
        }
    }
    Some(name)
}

/// The name the status line shows for `session`: none while the switch is
/// off; else its name ([`find`], its root stamped in once known,
/// [`keep_root`]), else the `name` on its manager feed ([`on_feed`]).
pub fn shown(feeds: &Path, session: &str, transcript: Option<&Path>) -> Option<String> {
    if !is_on(feeds) {
        return None;
    }
    keep_root(feeds, session, transcript)
        .or_else(|| on_feed(feeds, session))
        .or_else(|| find(feeds, session, transcript))
}

/// `session`'s room (giverny#435): the one recorded beside its name, else
/// its conversation's (another id of it, by the root its document holds),
/// else, for a document from before rooms, one drawn now and recorded under
/// both, so it is drawn once. None for a session with no name recorded.
pub fn room(feeds: &Path, session: &str) -> Option<String> {
    let file = session_file(feeds, session)?;
    let mut doc = read_doc(&file)?;
    doc_name(&doc)?;
    if let Some(room) = doc_room(&doc) {
        return Some(room);
    }
    // Drawn and written in one step (giverny#441): two looks at once, or a
    // root stamped meanwhile ([`keep_root`]), draw it once.
    let _lock = lock(feeds);
    doc = read_doc(&file)?;
    if let Some(room) = doc_room(&doc) {
        return Some(room);
    }
    let root = doc.get("root").and_then(Value::as_str).map(String::from);
    let rf = root.as_deref().and_then(|r| root_file(feeds, r));
    let mut root_doc = rf.as_deref().and_then(read_doc);
    let room = root_doc
        .as_ref()
        .and_then(doc_room)
        .unwrap_or_else(draw_room);
    doc["room"] = json!(room);
    let _ = write_doc(&file, &doc);
    if let (Some(rf), Some(rd)) = (rf, root_doc.as_mut())
        && doc_room(rd).is_none()
    {
        rd["room"] = json!(room);
        let _ = write_doc(&rf, rd);
    }
    Some(room)
}

/// The place a named `session`'s tab is titled after (giverny#435):
/// `Rotem's Workshop`, its first name and its [`room`]. None while the
/// switch is off ([`is_on`]) or the session has no name recorded under its
/// id: its tab keeps Claude's own title.
pub fn place(feeds: &Path, session: &str) -> Option<String> {
    if !is_on(feeds) {
        return None;
    }
    let name = read(feeds, session).or_else(|| adopt(feeds, session))?;
    place_of(&name, &room(feeds, session)?)
}

/// Give `session`, named only on its manager feed (from before names had a
/// home, giverny#330), a names document of that name, so it has a room to
/// be titled after (giverny#440). Nothing writes a feed's name any more, so
/// the copy never goes stale; the status line stamps its root in
/// ([`keep_root`]).
fn adopt(feeds: &Path, session: &str) -> Option<String> {
    let name = on_feed(feeds, session)?;
    let file = session_file(feeds, session)?;
    let _lock = lock(feeds);
    if let Some(name) = name_in(&file) {
        return Some(name);
    }
    write_doc(
        &file,
        &json!({"session": session, "name": name, "room": draw_room()}),
    )
    .ok()?;
    Some(name)
}

/// One word of a name: a letter, then letters, digits, hyphens and
/// apostrophes; 40 characters at most.
fn is_word(w: &str) -> bool {
    let mut chars = w.chars();
    chars.next().is_some_and(char::is_alphabetic)
        && chars.all(|c| c.is_alphanumeric() || c == '-' || c == '\'')
        && w.chars().count() <= 40
}

/// The full name `given` makes: two words are kept as given; one word keeps
/// `previous` when its first name is the same (case aside), else takes the
/// session's surname, `surname()` ([`surname`], giverny#434).
pub fn full_name(
    given: &str,
    previous: Option<&str>,
    surname: impl FnOnce() -> String,
) -> Result<String, String> {
    let words: Vec<&str> = given.split_whitespace().collect();
    let bad = || {
        format!(
            "a name is a first name, or a first name and a surname \
             (letters, digits, hyphens, apostrophes): got {given:?}"
        )
    };
    match words.as_slice() {
        [first, last] if is_word(first) && is_word(last) => Ok(format!("{first} {last}")),
        [first] if is_word(first) => {
            // The same first name, case aside: the name stands as recorded.
            let kept = previous.filter(|p| {
                let mut w = p.split_whitespace();
                w.next()
                    .is_some_and(|pf| pf.to_lowercase() == first.to_lowercase())
                    && w.next().is_some()
            });
            Ok(match kept {
                Some(p) => p.split_whitespace().collect::<Vec<_>>().join(" "),
                None => format!("{first} {}", surname()),
            })
        }
        _ => Err(bad()),
    }
}

/// One of `list`, at random: the std hasher's random keys over the clock and
/// the process id, enough to spread names and nothing to build a crate on.
fn pick(list: &[&str]) -> String {
    use std::hash::BuildHasher;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = std::collections::hash_map::RandomState::new().hash_one((seed, std::process::id()));
    list[(n % list.len() as u64) as usize].to_string()
}

/// A first name from [`FIRST_NAMES`], at random.
pub fn draw_first_name() -> String {
    pick(FIRST_NAMES)
}

/// The sessions live now, each with the directory its claude runs in.
pub type Live = std::collections::HashMap<String, PathBuf>;

/// The sessions whose claude runs now: Claude Code's own registry
/// (`sessions/<pid>.json`, pid-checked, [`crate::registry::scan`]) in every
/// account on the machine, `cfg` first when given. What the panel and the
/// reach guard call live; a session gone is gone from it, whatever its
/// files say.
pub fn live_sessions(cfg: Option<&Path>) -> Live {
    crate::registry::scan(crate::registry::ambient_config_dirs(cfg))
        .into_iter()
        .map(|s| (s.entry.session_id, s.entry.cwd))
        .collect()
}

/// A live session and the name it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub session: String,
    pub name: String,
    /// Where its claude runs: the repo, or the worktree its task is in.
    pub cwd: PathBuf,
}

impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix: String = self.session.chars().take(8).collect();
        write!(f, "{} (session {prefix}", self.name)?;
        if !self.cwd.as_os_str().is_empty() {
            let cwd = match dirs::home_dir().and_then(|h| self.cwd.strip_prefix(h).ok()) {
                Some(rel) => Path::new("~").join(rel),
                None => self.cwd.clone(),
            };
            write!(f, ", in {}", cwd.display())?;
        }
        write!(f, ")")
    }
}

/// Every `live` session but `session` itself that holds a name: its own
/// ([`DIR`]), else the one on its manager feed. A session of the same
/// conversation (`root`: a resume's other id, a job's tab) is `session`
/// itself under another id, so it holds nothing against it.
fn holders(feeds: &Path, live: &Live, session: &str, root: Option<&str>) -> Vec<Holder> {
    let mut out: Vec<Holder> = live
        .iter()
        .filter(|(id, _)| id.as_str() != session)
        .filter_map(|(id, cwd)| {
            let doc = session_file(feeds, id).and_then(|f| read_doc(&f));
            let theirs = doc.as_ref().and_then(|d| d.get("root")?.as_str());
            if root.is_some() && theirs == root {
                return None;
            }
            let name = doc
                .as_ref()
                .and_then(doc_name)
                .or_else(|| on_feed(feeds, id))?;
            Some(Holder {
                session: id.clone(),
                name,
                cwd: cwd.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.session.cmp(&b.session));
    out
}

/// The sessions not in `live` whose names document was written in the
/// last `within` (giverny#433): just named, and live by now though
/// Claude Code's registry may not list them yet. `session` itself and
/// another id of its conversation (`root`) hold nothing against it. What a
/// draw steers clear of beside [`holders`]; a name given by hand is checked
/// against the live ones alone.
fn fresh(
    feeds: &Path,
    live: &Live,
    session: &str,
    root: Option<&str>,
    within: std::time::Duration,
) -> Vec<Holder> {
    let now = std::time::SystemTime::now();
    let Ok(entries) = std::fs::read_dir(dir(feeds)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_str()?.to_string();
            if path.extension()? != "json" || id == session || live.contains_key(&id) {
                return None;
            }
            let age = now.duration_since(e.metadata().ok()?.modified().ok()?);
            if age.is_ok_and(|a| a > within) {
                return None;
            }
            let doc = read_doc(&path)?;
            if root.is_some() && doc.get("root").and_then(Value::as_str) == root {
                return None;
            }
            Some(Holder {
                session: id,
                name: doc_name(&doc)?,
                cwd: PathBuf::new(),
            })
        })
        .collect()
}

/// A name's first name, for comparing: case aside.
fn first_of(name: &str) -> Option<String> {
    name.split_whitespace().next().map(str::to_lowercase)
}

/// The holder whose first name is `name`'s, if any.
fn held_by<'a>(held: &'a [Holder], name: &str) -> Option<&'a Holder> {
    let first = first_of(name)?;
    held.iter()
        .find(|h| first_of(&h.name).as_deref() == Some(first.as_str()))
}

/// A full name for a session that has none (giverny#364): a first name no
/// session in `held` holds (giverny#425: drawn from the pool less theirs,
/// not hoped for), and its `surname` ([`surname`], giverny#434). Every first
/// name held (hundreds of live sessions) draws from the whole pool. Two
/// sessions may share a full name over time (a second Rotem Giverny once the
/// first has ended); no two live ones share a first name.
pub fn draw_name(held: &[Holder], surname: &str) -> String {
    let free: Vec<&str> = FIRST_NAMES
        .iter()
        .copied()
        .filter(|f| held_by(held, f).is_none())
        .collect();
    let pool: &[&str] = if free.is_empty() { FIRST_NAMES } else { &free };
    format!("{} {surname}", pick(pool))
}

/// Record `session`'s name from `given` ([`full_name`]), under its id and,
/// when known, its conversation's `root`. Returns the full name and the one
/// it replaced. Refused when another live session ([`live_sessions`]) holds
/// its first name (giverny#425), naming who; `session` re-taking its own,
/// under this id or another of its conversation, is fine.
pub fn record(
    feeds: &Path,
    session: &str,
    given: &str,
    root: Option<&str>,
    surname: impl FnOnce() -> String,
) -> Result<(String, Option<String>), String> {
    record_among(feeds, session, given, root, surname, &live_sessions(None))
}

/// [`record`] against the `live` sessions given.
pub fn record_among(
    feeds: &Path,
    session: &str,
    given: &str,
    root: Option<&str>,
    surname: impl FnOnce() -> String,
    live: &Live,
) -> Result<(String, Option<String>), String> {
    // Who holds what, read and written in one step (giverny#433).
    let _lock = lock(feeds);
    record_held(
        feeds,
        session,
        given,
        root,
        surname,
        &holders(feeds, live, session, root),
    )
}

/// [`record`], refused when a session in `held` holds the first name.
fn record_held(
    feeds: &Path,
    session: &str,
    given: &str,
    root: Option<&str>,
    surname: impl FnOnce() -> String,
    held: &[Holder],
) -> Result<(String, Option<String>), String> {
    let file =
        session_file(feeds, session).ok_or_else(|| format!("not a session id: {session:?}"))?;
    let docs = [
        read_doc(&file),
        root.and_then(|r| read_doc(&root_file(feeds, r)?)),
    ];
    let previous = docs.iter().flatten().find_map(doc_name);
    let name = full_name(given, previous.as_deref(), surname)?;
    if let Some(h) = held_by(held, &name) {
        let first = name.split_whitespace().next().unwrap_or(&name);
        return Err(format!(
            "{first} is taken: {h} holds it, and no two live sessions share a first name; \
             give another, or run `giverny-manage name` alone to be drawn one"
        ));
    }
    // The room is drawn at birth and kept through any renaming
    // (giverny#435): the tab's title is the session's place, not its name
    // of the moment.
    let room = docs
        .iter()
        .flatten()
        .find_map(doc_room)
        .unwrap_or_else(draw_room);
    let mut doc = json!({"session": session, "name": name, "room": room});
    if let Some(r) = root {
        doc["root"] = json!(r);
    }
    write_doc(&file, &doc).map_err(|e| format!("{}: {e}", file.display()))?;
    if let Some(rf) = root.and_then(|r| root_file(feeds, r)) {
        write_doc(&rf, &doc).map_err(|e| format!("{}: {e}", rf.display()))?;
    }
    Ok((name, previous))
}

/// Draw `session` a first name and record it with its `surname`
/// ([`draw_name`]), under its conversation's root when its `transcript` has
/// one: what a session with no name gets, from `giverny-manage name` or its
/// start (giverny#401).
pub fn draw_and_record(
    feeds: &Path,
    session: &str,
    transcript: Option<&Path>,
    surname: &str,
) -> Result<String, String> {
    draw_and_record_among(feeds, session, transcript, surname, &live_sessions(None))
}

/// [`draw_and_record`] against the `live` sessions given: the first name
/// drawn is none of theirs.
pub fn draw_and_record_among(
    feeds: &Path,
    session: &str,
    transcript: Option<&Path>,
    surname: &str,
    live: &Live,
) -> Result<String, String> {
    let root = transcript.and_then(crate::subagents::conversation_root);
    // Drawn and recorded in one step (giverny#433): a session drawing at
    // the same instant draws after, and sees this one's name.
    let _lock = lock(feeds);
    let mut held = holders(feeds, live, session, root.as_deref());
    held.extend(fresh(feeds, live, session, root.as_deref(), FRESH));
    // A full name: `record` keeps it as given and adds nothing. Drawn
    // clear of `held` already, so checked against nobody: with every first
    // name held, the draw from the whole pool still stands.
    record_held(
        feeds,
        session,
        &draw_name(&held, surname),
        root.as_deref(),
        || surname.to_string(),
        &[],
    )
    .map(|(name, _)| name)
}

/// What a session in a Giverny tab is told at its start: its name (drawn
/// there unless it had one, giverny#401), else, only when recording it
/// failed, to ask Giverny for one (giverny#364: Giverny draws the whole
/// name, nothing is picked).
pub fn ask(name: Option<&str>) -> String {
    match name {
        Some(n) => format!(
            "Your name in this session is {n} (Giverny shows it on the status line; \
             `giverny-manage name` prints it)."
        ),
        None => "Giverny names its sessions: run `giverny-manage name` and it gives you your \
                 name, a first name and a surname; run it again any time to read it back."
            .to_string(),
    }
}

/// The session a hook payload is for, as a usable id.
fn session_of(payload: &Value) -> Option<&str> {
    payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| safe(s))
}

/// The working directory a hook payload names.
fn cwd_of(payload: &Value) -> Option<PathBuf> {
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
}

/// The transcript a hook payload names.
fn transcript_of(payload: &Value) -> Option<PathBuf> {
    payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(PathBuf::from)
}

/// Name `session` and say so ([`ask`]): the name it has ([`find`], else
/// the one on its manager feed), else one drawn and recorded right here
/// ([`draw_and_record`], giverny#401) with the surname of the tab it was
/// born in ([`surname`], giverny#434), so the session has its name before
/// its first turn. Marked as told, so it is not told again ([`ASKED`]).
fn tell(payload: &Value, feeds: &Path, session: &str) -> String {
    let transcript = transcript_of(payload);
    let name = find(feeds, session, transcript.as_deref())
        .or_else(|| on_feed(feeds, session))
        .or_else(|| {
            let born = surname(feeds, cwd_of(payload).as_deref());
            draw_and_record(feeds, session, transcript.as_deref(), &born)
                .inspect_err(|e| eprintln!("giverny hook: naming {session}: {e}"))
                .ok()
        });
    if let Some(file) = asked_file(feeds, session) {
        let _ = std::fs::create_dir_all(dir(feeds).join(ASKED));
        let _ = std::fs::write(file, "");
    }
    ask(name.as_deref())
}

/// The `SessionStart` hook's reply: [`start_context`] as
/// `additionalContext`.
pub fn session_start(payload: &Value, feeds: &Path, on: bool, in_tab: bool) -> Option<String> {
    let context = start_context(payload, feeds, on, in_tab)?;
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": context
            }
        })
        .to_string(),
    )
}

/// What a session start is told, naming it first ([`tell`]): only with the
/// switch `on`, only for a top-level session (`in_tab`: a Giverny tab's own,
/// not a background job's or a nested claude's), never a subagent's. Every
/// start tells again (a `/clear` is a new session and draws a new name; a
/// resume or a compaction keeps its name), and is marked so the prompt hook
/// stays quiet ([`first_prompt`]). The text alone, for a hook reply that
/// carries more (giverny#373).
pub fn start_context(payload: &Value, feeds: &Path, on: bool, in_tab: bool) -> Option<String> {
    if !on || !in_tab || payload.get("agent_id").is_some() {
        return None;
    }
    let session = session_of(payload)?;
    Some(tell(payload, feeds, session))
}

/// The naming a `UserPromptSubmit` carries when the session's start did
/// not: its claude predates the plugin's `SessionStart` hook, or the switch
/// was off then (giverny#356). It names the session as its start would have
/// ([`tell`], giverny#401). The same gates as [`session_start`], plus once
/// per session; `in_tab` is asked only for a session not yet told, as it
/// walks the process tree. A session told already has its root stamped in
/// instead, once its transcript has one ([`keep_root`]). Returns the context
/// text, for the prompt hook's reply.
pub fn first_prompt(
    payload: &Value,
    feeds: &Path,
    on: bool,
    in_tab: impl FnOnce() -> bool,
) -> Option<String> {
    if !on || payload.get("agent_id").is_some() {
        return None;
    }
    let session = session_of(payload)?;
    if asked_file(feeds, session)?.exists() {
        keep_root(feeds, session, transcript_of(payload).as_deref());
        return None;
    }
    if !in_tab() {
        return None;
    }
    Some(tell(payload, feeds, session))
}

/// A `SessionStart` hook payload, parsed; `None` for any other event (read
/// whole only when it names that one: the others can be large).
pub fn start_payload(input: &str) -> Option<Value> {
    if !input.contains("\"SessionStart\"") {
        return None;
    }
    let v: Value = serde_json::from_str(input).ok()?;
    (v.get("hook_event_name").and_then(Value::as_str) == Some("SessionStart")).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-names-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn drawn(s: &str) -> impl FnOnce() -> String + '_ {
        move || s.to_string()
    }

    /// Every name recorded under a session id in `feeds`' names directory.
    fn recorded(feeds: &Path) -> std::collections::HashSet<String> {
        std::fs::read_dir(dir(feeds))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| name_in(&p))
            .collect()
    }

    /// An environment of `vars` alone.
    fn env_of<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn a_first_name_gets_a_drawn_surname_once_and_keeps_it() {
        let feeds = scratch("draw");
        let (n, was) = record(&feeds, "s1", "Tamsin", Some("r1"), drawn("Camden")).unwrap();
        assert_eq!((n.as_str(), was), ("Tamsin Camden", None));
        // The same first name again: the surname stands, nothing is drawn.
        let (n, was) = record(&feeds, "s1", "tamsin", Some("r1"), || panic!("no draw")).unwrap();
        assert_eq!(n, "Tamsin Camden");
        assert_eq!(was.as_deref(), Some("Tamsin Camden"));
        // Another first name is another person: a new surname.
        let (n, _) = record(&feeds, "s1", "Wren", Some("r1"), drawn("Ashby")).unwrap();
        assert_eq!(n, "Wren Ashby");
        assert_eq!(read(&feeds, "s1").as_deref(), Some("Wren Ashby"));
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_full_name_is_kept_as_given() {
        let feeds = scratch("full");
        let (n, _) = record(&feeds, "s1", "Tamsin  Camden", None, || panic!("no draw")).unwrap();
        assert_eq!(n, "Tamsin Camden");
        // A revived name over a drawn one: the given surname wins.
        record(&feeds, "s2", "Pike", None, drawn("Ashby")).unwrap();
        let (n, _) = record(&feeds, "s2", "Pike Vance", None, || panic!()).unwrap();
        assert_eq!(n, "Pike Vance");
        for bad in ["", "Three Word Name", "9lives", "Tam/sin", "Tamsin Cam$den"] {
            assert!(
                record(&feeds, "s3", bad, None, drawn("X")).is_err(),
                "{bad:?}"
            );
        }
        assert!(read(&feeds, "s3").is_none());
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_drawn_name_is_a_first_name_and_the_sessions_surname() {
        // giverny#364: Giverny draws the first name from its own list;
        // giverny#434: the surname is the session's category, not drawn.
        assert!(FIRST_NAMES.len() >= 300, "a few hundred");
        assert!(FIRST_NAMES.iter().all(|s| is_word(s) && s.is_ascii()));
        let unique: std::collections::HashSet<_> = FIRST_NAMES.iter().collect();
        assert_eq!(unique.len(), FIRST_NAMES.len(), "no first name twice");
        assert!(FIRST_NAMES.contains(&draw_first_name().as_str()));

        let feeds = scratch("drawn");
        let drawn = draw_name(&[], "Giverny");
        let (first, last) = drawn.split_once(' ').unwrap();
        assert!(FIRST_NAMES.contains(&first), "{drawn}");
        assert_eq!(last, "Giverny");
        // Recorded as drawn: a full name, kept as given.
        let (n, _) = record(&feeds, "s1", &drawn, None, || panic!("no draw")).unwrap();
        assert_eq!(n, drawn);
        // Another session, later, in the same category may be the same full
        // name: only live first names are kept apart (giverny#425).
        let (n, _) =
            record_among(&feeds, "s2", first, None, || "Giverny".into(), &Live::new()).unwrap();
        assert_eq!(n, drawn);
        let name = draw_and_record_among(&feeds, "s3", None, "ToLaD", &Live::new()).unwrap();
        assert!(name.ends_with(" ToLaD"), "{name}");
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_category_as_a_surname_is_kept_readable() {
        for (category, surname) in [
            ("giverny", "Giverny"),
            ("ToLaD", "ToLaD"),
            ("COO", "COO"),
            ("lead-machine", "Lead-machine"),
            ("lead machine", "Lead-machine"),
            ("  Inbar  ", "Inbar"),
            ("WeatherBaseline", "WeatherBaseline"),
            ("3D prints", "D-prints"),
            ("café ☕ club", "Café-club"),
            ("poly/markets!", "Poly-markets"),
            ("o'brien's", "O'brien's"),
            ("élan", "Élan"),
        ] {
            assert_eq!(
                as_surname(category).as_deref(),
                Some(surname),
                "{category:?}"
            );
            assert!(is_word(surname), "{surname}");
            // A first name and it make a full name that is kept as given.
            assert_eq!(
                full_name("Rotem", None, || surname.to_string()),
                Ok(format!("Rotem {surname}"))
            );
        }
        for none in ["", "   ", "123", "☕☕", "--"] {
            assert_eq!(as_surname(none), None, "{none:?}");
        }
        let long = as_surname(&"a-".repeat(60)).unwrap();
        assert!(is_word(&long) && long.chars().count() <= 40, "{long}");
    }

    #[test]
    fn the_surname_is_the_tabs_category_else_its_repo() {
        let base = scratch("surname");
        let feeds = base.join("feeds");
        // A repo, and a worktree of it under its `.claude/worktrees`.
        let repo = base.join("ToLaD");
        std::fs::create_dir_all(repo.join(".git/worktrees/w1")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let wt = repo.join(".claude/worktrees/task-1");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}/.git/worktrees/w1\n", repo.display()),
        )
        .unwrap();
        let plain = base.join("scratch-dir");
        std::fs::create_dir_all(&plain).unwrap();

        // The category the tab's shell was started in.
        let tab = [(CATEGORY_ENV, "giverny"), ("GIVERNY_TAB_ID", "giverny-12")];
        let at = |vars: &[(&str, &str)], cwd: &Path| surname_in(&env_of(vars), &feeds, Some(cwd));
        assert_eq!(at(&tab, &repo), "Giverny", "the category, not the repo");
        assert_eq!(at(&[(CATEGORY_ENV, "lead machine")], &repo), "Lead-machine");
        // No category: the repo, a worktree counting as its main checkout,
        // else the directory's own name.
        assert_eq!(at(&[], &repo.join("src")), "ToLaD");
        assert_eq!(at(&[], &wt), "ToLaD");
        assert_eq!(at(&[], &plain), "Scratch-dir");
        assert_eq!(at(&[(CATEGORY_ENV, " ")], &repo), "ToLaD", "blank is none");
        assert_eq!(at(&[(CATEGORY_ENV, "☕")], &repo), "ToLaD", "no letter");

        // A shell started before Giverny set the variable: the tab's
        // category by the workspace the app saved beside its feeds.
        std::fs::create_dir_all(base.join("state")).unwrap();
        std::fs::write(
            base.join("state/tabs.json"),
            json!({"version": 1, "workspace": {
                "categories": [{"id": 3, "name": "WeatherBaseline"},
                               {"id": 21, "name": "inbar"}],
                "tabs": [{"id": 12, "category": 21}, {"id": 7, "category": 3}]}})
            .to_string(),
        )
        .unwrap();
        assert_eq!(at(&[("GIVERNY_TAB_ID", "giverny-12")], &repo), "Inbar");
        assert_eq!(
            at(&[("GIVERNY_TAB_ID", "giverny-7")], &repo),
            "WeatherBaseline"
        );
        assert_eq!(at(&[("GIVERNY_TAB_ID", "giverny-99")], &repo), "ToLaD");
        // giverny#439: the tab moved to inbar since its shell started in
        // giverny: where it sits now wins.
        assert_eq!(at(&tab, &repo), "Inbar");
        // A tab the saved workspace does not know: the shell's own word.
        assert_eq!(
            at(
                &[(CATEGORY_ENV, "giverny"), ("GIVERNY_TAB_ID", "giverny-99")],
                &repo
            ),
            "Giverny"
        );
        // A feed directory not the app's own has no workspace beside it.
        assert_eq!(
            surname_in(
                &env_of(&[("GIVERNY_TAB_ID", "giverny-12")]),
                &base,
                Some(&repo)
            ),
            "ToLaD"
        );
        // Inside a background job the tab's variables are some other tab's:
        // the repo.
        let job = [
            ("CLAUDE_JOB_DIR", "/home/x/.claude/jobs/abc"),
            (CATEGORY_ENV, "giverny"),
            ("GIVERNY_TAB_ID", "giverny-12"),
        ];
        assert_eq!(at(&job, &repo), "ToLaD");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_resumed_session_keeps_the_name_it_was_born_with() {
        // giverny#434: fixed at birth. One named before surnames were
        // categories keeps its drawn surname across a resume, and naming
        // again by its first name keeps it too.
        let feeds = scratch("born");
        set_on(&feeds, true).unwrap();
        record(&feeds, "old", "Rotem Ashby", Some("root-1"), || panic!()).unwrap();
        let t = feeds.join("new.jsonl");
        std::fs::write(
            &t,
            "{\"type\":\"user\",\"uuid\":\"root-1\",\"parentUuid\":null}\n",
        )
        .unwrap();
        let start = json!({"session_id": "new", "hook_event_name": "SessionStart",
                           "transcript_path": t.to_str().unwrap(), "source": "resume"});
        let told = session_start(&start, &feeds, true, true).unwrap();
        assert!(told.contains("Rotem Ashby"), "{told}");
        assert_eq!(read(&feeds, "new").as_deref(), Some("Rotem Ashby"));
        let (n, _) = record(&feeds, "new", "Rotem", Some("root-1"), drawn("Giverny")).unwrap();
        assert_eq!(n, "Rotem Ashby", "not renamed");
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn the_switch_hides_every_name() {
        let feeds = scratch("switch");
        record(&feeds, "s1", "Tamsin Camden", None, || panic!()).unwrap();
        assert!(!is_on(&feeds), "absent means off");
        assert_eq!(shown(&feeds, "s1", None), None);
        assert!(set_on(&feeds, true).unwrap());
        assert!(!set_on(&feeds, true).unwrap(), "a no-op the second time");
        assert_eq!(shown(&feeds, "s1", None).as_deref(), Some("Tamsin Camden"));
        assert!(set_on(&feeds, false).unwrap());
        assert_eq!(shown(&feeds, "s1", None), None);
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_resumed_session_finds_its_name_by_its_conversation() {
        let feeds = scratch("resume");
        set_on(&feeds, true).unwrap();
        record(&feeds, "old", "Tamsin", Some("root-1"), drawn("Camden")).unwrap();
        // The new id's transcript carries the conversation forward.
        let t = feeds.join("new.jsonl");
        std::fs::write(
            &t,
            "{\"type\":\"user\",\"uuid\":\"root-1\",\"parentUuid\":null}\n",
        )
        .unwrap();
        assert_eq!(read(&feeds, "new"), None);
        assert_eq!(
            shown(&feeds, "new", Some(&t)).as_deref(),
            Some("Tamsin Camden")
        );
        assert_eq!(read(&feeds, "new").as_deref(), Some("Tamsin Camden"));
        // Naming again under the new id keeps the surname.
        let (n, _) = record(&feeds, "new", "Tamsin", Some("root-1"), || panic!()).unwrap();
        assert_eq!(n, "Tamsin Camden");
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn the_name_is_given_only_with_the_switch_on_and_never_to_a_subagent() {
        let feeds = scratch("ask");
        let start = |extra: Value| {
            let mut v = json!({"session_id": "s1", "hook_event_name": "SessionStart",
                               "source": "startup"});
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            v
        };
        let context = |out: String| {
            let v: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let p = start(json!({}));
        assert_eq!(session_start(&p, &feeds, false, true), None, "switch off");
        assert_eq!(
            session_start(&p, &feeds, true, false),
            None,
            "not a tab's own"
        );
        let sub = start(json!({"agent_id": "a1"}));
        assert_eq!(session_start(&sub, &feeds, true, true), None, "a subagent");
        assert_eq!(read(&feeds, "s1"), None, "nobody named yet");
        assert!(recorded(&feeds).is_empty());
        // A fresh start is named right there, before any turn (giverny#401).
        let told = context(session_start(&p, &feeds, true, true).unwrap());
        let name = read(&feeds, "s1").expect("recorded at the start");
        let (first, last) = name.split_once(' ').unwrap();
        assert!(FIRST_NAMES.contains(&first), "{name}");
        assert_eq!(last, surname(&feeds, None), "the session's own surname");
        assert!(
            told.starts_with(&format!("Your name in this session is {name} ")),
            "{told}"
        );
        // Giverny gives the name: nothing to run, nothing to pick
        // (giverny#364).
        assert!(!told.contains("run `giverny-manage name`"), "{told}");
        assert!(
            !told.contains("pick") && !told.contains("<First>"),
            "{told}"
        );
        assert!(!told.contains('\n'), "one line");
        // Started again (a compaction): the same name, nothing drawn.
        let again = context(session_start(&p, &feeds, true, true).unwrap());
        assert_eq!(again, told);
        assert_eq!(recorded(&feeds).len(), 1);
        // The hook reads only a SessionStart payload.
        assert!(start_payload(&p.to_string()).is_some());
        assert!(start_payload(r#"{"hook_event_name":"PostToolUse"}"#).is_none());
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_session_whose_start_ran_no_hook_is_asked_on_its_next_prompt_once() {
        // giverny#356: a claude started before the plugin carried its
        // SessionStart hook, or with the switch off, runs only the prompt
        // hook; that asks, once per session.
        let feeds = scratch("first-prompt");
        let prompt = |session: &str| {
            json!({"session_id": session, "hook_event_name": "UserPromptSubmit",
                   "prompt": "hi"})
        };
        let yes = || true;
        let p = prompt("s1");
        assert_eq!(first_prompt(&p, &feeds, false, yes), None, "switch off");
        assert_eq!(
            first_prompt(&p, &feeds, true, || false),
            None,
            "not a tab's own"
        );
        let mut sub = prompt("s1");
        sub["agent_id"] = json!("a1");
        assert_eq!(first_prompt(&sub, &feeds, true, yes), None, "a subagent");
        assert_eq!(read(&feeds, "s1"), None, "nobody named yet");
        // Named on the prompt, as its start would have (giverny#401).
        let asked = first_prompt(&p, &feeds, true, yes).unwrap();
        let name = read(&feeds, "s1").expect("recorded on the prompt");
        assert!(asked.contains(&name), "{asked}");
        assert!(!asked.contains("run `giverny-manage name`"), "{asked}");
        assert_eq!(
            first_prompt(&p, &feeds, true, || panic!("no walk once told")),
            None,
            "asked once"
        );
        // A session its start named already is not told again, nor renamed.
        let start = json!({"session_id": "s2", "hook_event_name": "SessionStart",
                           "source": "clear"});
        assert!(session_start(&start, &feeds, true, true).is_some());
        let s2 = read(&feeds, "s2").unwrap();
        assert_eq!(first_prompt(&prompt("s2"), &feeds, true, yes), None);
        assert_eq!(read(&feeds, "s2"), Some(s2));
        // One named already is told its name, as its start would have.
        record(&feeds, "s3", "Tamsin Camden", None, || panic!()).unwrap();
        let told = first_prompt(&prompt("s3"), &feeds, true, yes).unwrap();
        assert!(told.contains("Tamsin Camden"), "{told}");
        // Off at its start, so neither named nor marked: on later, its next
        // prompt names it.
        let s4 = json!({"session_id": "s4", "hook_event_name": "SessionStart"});
        assert_eq!(session_start(&s4, &feeds, false, true), None);
        assert_eq!(read(&feeds, "s4"), None);
        assert!(first_prompt(&prompt("s4"), &feeds, true, yes).is_some());
        assert!(read(&feeds, "s4").is_some());
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_session_named_at_its_start_keeps_its_name_across_a_resume() {
        // giverny#401: named before its first turn, a session has no root
        // yet; its first prompt or the status line stamps one in once the
        // transcript has it, and a resume (a new id) finds the name by it.
        let feeds = scratch("start-resume");
        set_on(&feeds, true).unwrap();
        let t = feeds.join("s1.jsonl");
        let payload = |session: &str, event: &str| {
            json!({"session_id": session, "hook_event_name": event,
                   "transcript_path": t.to_str().unwrap()})
        };
        // A fresh start: no transcript yet, so no root.
        session_start(&payload("s1", "SessionStart"), &feeds, true, true).unwrap();
        let name = read(&feeds, "s1").unwrap();
        assert!(read_doc(&session_file(&feeds, "s1").unwrap()).unwrap()["root"].is_null());
        // A prompt before the transcript has a turn: still nothing to stamp.
        assert_eq!(
            first_prompt(&payload("s1", "UserPromptSubmit"), &feeds, true, || true),
            None
        );
        assert!(!dir(&feeds).join("roots").exists());
        // The first turn lands; the next prompt stamps the root.
        std::fs::write(
            &t,
            "{\"type\":\"user\",\"uuid\":\"root-1\",\"parentUuid\":null}\n",
        )
        .unwrap();
        first_prompt(&payload("s1", "UserPromptSubmit"), &feeds, true, || true);
        assert_eq!(
            name_in(&root_file(&feeds, "root-1").unwrap()).as_deref(),
            Some(name.as_str())
        );
        // Resumed under a new id, the same transcript's conversation: the
        // same name, nothing drawn.
        let told = session_start(&payload("s2", "SessionStart"), &feeds, true, true).unwrap();
        assert!(told.contains(&name), "{told}");
        assert_eq!(read(&feeds, "s2"), Some(name.clone()));
        assert_eq!(recorded(&feeds).len(), 1, "one name, under two ids");

        // The status line stamps it too, for a session with no prompt hook.
        let u = feeds.join("s3.jsonl");
        session_start(
            &json!({"session_id": "s3", "hook_event_name": "SessionStart",
                    "transcript_path": u.to_str().unwrap()}),
            &feeds,
            true,
            true,
        )
        .unwrap();
        let s3 = read(&feeds, "s3").unwrap();
        assert_eq!(shown(&feeds, "s3", Some(&u)), Some(s3.clone()));
        std::fs::write(
            &u,
            "{\"type\":\"user\",\"uuid\":\"root-3\",\"parentUuid\":null}\n",
        )
        .unwrap();
        assert_eq!(shown(&feeds, "s3", Some(&u)), Some(s3.clone()));
        assert_eq!(name_in(&root_file(&feeds, "root-3").unwrap()), Some(s3));
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_room_is_drawn_at_birth_and_kept() {
        // giverny#435: drawn once, recorded beside the name, and the same
        // across reads, a renaming and a resume.
        let feeds = scratch("room");
        record(&feeds, "s1", "Tamsin", Some("root-1"), drawn("Giverny")).unwrap();
        let doc = read_doc(&session_file(&feeds, "s1").unwrap()).unwrap();
        let born = doc_room(&doc).expect("a room at birth");
        assert!(ROOMS.contains(&born.as_str()), "{born}");
        for _ in 0..5 {
            assert_eq!(room(&feeds, "s1").as_deref(), Some(born.as_str()));
        }
        assert_eq!(place(&feeds, "s1"), None, "the switch is off");
        set_on(&feeds, true).unwrap();
        assert_eq!(place(&feeds, "s1"), Some(format!("Tamsin's {born}")));
        // Renamed: another first name, the same room.
        record(&feeds, "s1", "Xanthe", Some("root-1"), drawn("Giverny")).unwrap();
        assert_eq!(place(&feeds, "s1"), Some(format!("Xanthe's {born}")));
        // Resumed under a new id: the conversation's room comes with its name.
        let t = feeds.join("new.jsonl");
        std::fs::write(
            &t,
            "{\"type\":\"user\",\"uuid\":\"root-1\",\"parentUuid\":null}\n",
        )
        .unwrap();
        assert_eq!(
            shown(&feeds, "new", Some(&t)).as_deref(),
            Some("Xanthe Giverny")
        );
        assert_eq!(place(&feeds, "new"), Some(format!("Xanthe's {born}")));
        // Nobody named, no place.
        assert_eq!(place(&feeds, "nobody"), None);
        assert_eq!(room(&feeds, "nobody"), None);
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_name_from_before_rooms_is_given_one_once() {
        // giverny#435: a document written before rooms draws one on first
        // need and records it under its id and its conversation's root, so
        // another id of the conversation takes the same.
        let feeds = scratch("old-room");
        let old = json!({"session": "s1", "name": "Rotem Ashby", "root": "root-1"});
        write_doc(&session_file(&feeds, "s1").unwrap(), &old).unwrap();
        write_doc(&root_file(&feeds, "root-1").unwrap(), &old).unwrap();
        let other = json!({"session": "s2", "name": "Rotem Ashby", "root": "root-1"});
        write_doc(&session_file(&feeds, "s2").unwrap(), &other).unwrap();
        let drawn_now = room(&feeds, "s1").expect("drawn");
        assert!(ROOMS.contains(&drawn_now.as_str()));
        let on_disk = |f: PathBuf| doc_room(&read_doc(&f).unwrap());
        assert_eq!(
            on_disk(session_file(&feeds, "s1").unwrap()),
            Some(drawn_now.clone())
        );
        assert_eq!(
            on_disk(root_file(&feeds, "root-1").unwrap()),
            Some(drawn_now.clone())
        );
        assert_eq!(room(&feeds, "s1"), Some(drawn_now.clone()), "drawn once");
        assert_eq!(
            room(&feeds, "s2"),
            Some(drawn_now.clone()),
            "its conversation's"
        );
        // Naming again keeps it too.
        record(&feeds, "s1", "Rotem", Some("root-1"), || panic!()).unwrap();
        assert_eq!(room(&feeds, "s1"), Some(drawn_now));
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_place_is_the_first_name_and_the_room() {
        assert_eq!(
            place_of("Rotem Giverny", "Workshop").as_deref(),
            Some("Rotem's Workshop")
        );
        assert_eq!(
            place_of("Silas ToLaD", "Loft").as_deref(),
            Some("Silas's Loft")
        );
        assert_eq!(place_of("", "Loft"), None);
        assert_eq!(place_of("Rotem Giverny", " "), None);
        assert!(
            ROOMS.iter().all(|r| is_word(r) && r.len() <= 12),
            "short words"
        );
    }

    /// A live map: each session with where it runs.
    fn live_of(ids: &[&str]) -> Live {
        ids.iter()
            .map(|id| (id.to_string(), PathBuf::from(format!("/work/{id}"))))
            .collect()
    }

    #[test]
    fn a_drawn_first_name_is_none_a_live_session_holds() {
        // giverny#425: two live managers were both Rotem. Every first name
        // but the last is held by a live session: the draw is the last,
        // every time, not a hope of redraws.
        let feeds = scratch("live-draw");
        let (last, held) = FIRST_NAMES.split_last().unwrap();
        let ids: Vec<String> = (0..held.len()).map(|i| format!("h{i}")).collect();
        for (id, first) in ids.iter().zip(held) {
            record_held(
                &feeds,
                id,
                &format!("{first} Ashby"),
                None,
                || panic!(),
                &[],
            )
            .unwrap();
        }
        let mut live = live_of(&ids.iter().map(String::as_str).collect::<Vec<_>>());
        for _ in 0..20 {
            let name = draw_name(&holders(&feeds, &live, "me", None), "Giverny");
            assert_eq!(name.split_once(' ').unwrap().0, *last, "{name}");
        }
        let name = draw_and_record_among(&feeds, "me", None, "Giverny", &live).unwrap();
        assert!(name.starts_with(&format!("{last} ")), "{name}");
        // Its session ended: h0's first name is free again (two now, the
        // last being taken by "me" is not live), once its name is no
        // longer fresh (giverny#433).
        live.remove("h0");
        std::fs::File::options()
            .write(true)
            .open(session_file(&feeds, "h0").unwrap())
            .unwrap()
            .set_modified(std::time::SystemTime::now() - 2 * FRESH)
            .unwrap();
        live.insert("me".into(), PathBuf::new());
        let name = draw_and_record_among(&feeds, "you", None, "Giverny", &live).unwrap();
        assert!(name.starts_with(&format!("{} ", held[0])), "{name}");
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn live_is_a_running_claude_by_the_registry() {
        let feeds = scratch("registry");
        let cfg = feeds.join("claude");
        std::fs::create_dir_all(cfg.join("sessions")).unwrap();
        let entry = |pid: u32, sid: &str| {
            std::fs::write(
                cfg.join("sessions").join(format!("{pid}.json")),
                json!({"pid": pid, "sessionId": sid, "cwd": "/home/x/Inbar"}).to_string(),
            )
            .unwrap();
        };
        entry(std::process::id(), "names-test-live");
        entry(4_294_967_000, "names-test-dead");
        let live = live_sessions(Some(&cfg));
        assert_eq!(
            live.get("names-test-live"),
            Some(&PathBuf::from("/home/x/Inbar"))
        );
        assert!(!live.contains_key("names-test-dead"), "its pid is gone");
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_first_name_a_live_session_holds_is_refused_by_hand() {
        let feeds = scratch("hand");
        record_held(
            &feeds,
            "819f7e9cabc",
            "Rotem Tennyson",
            Some("r9"),
            || panic!(),
            &[],
        )
        .unwrap();
        let live = live_of(&["819f7e9cabc", "me"]);
        for given in ["Rotem", "rotem Ashby", "ROTEM"] {
            let e = record_among(&feeds, "me", given, None, drawn("Ashby"), &live).unwrap_err();
            assert!(
                e.contains("Rotem Tennyson (session 819f7e9c, in /work/819f7e9cabc)"),
                "{e}"
            );
        }
        assert_eq!(read(&feeds, "me"), None, "nothing recorded");
        // Another first name is fine.
        let (n, _) = record_among(&feeds, "me", "Wren", None, drawn("Ashby"), &live).unwrap();
        assert_eq!(n, "Wren Ashby");
        // The holder gone, its first name is free.
        let gone = live_of(&["me"]);
        let (n, _) = record_among(&feeds, "you", "Rotem", None, drawn("Ashby"), &gone).unwrap();
        assert_eq!(n, "Rotem Ashby");
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_session_re_takes_its_own_name() {
        let feeds = scratch("own");
        record_held(&feeds, "me", "Rotem Ashby", Some("r1"), || panic!(), &[]).unwrap();
        // Itself, live: naming again, by first name or in full, is fine.
        let live = live_of(&["me"]);
        let (n, _) = record_among(&feeds, "me", "Rotem", Some("r1"), || panic!(), &live).unwrap();
        assert_eq!(n, "Rotem Ashby");
        record_among(&feeds, "me", "Rotem Ashby", Some("r1"), || panic!(), &live).unwrap();
        // Revived under a new id while the old one still runs (the same
        // conversation): its own name, not a clash.
        let both = live_of(&["me", "new"]);
        let (n, _) =
            record_among(&feeds, "new", "Rotem Ashby", Some("r1"), || panic!(), &both).unwrap();
        assert_eq!(n, "Rotem Ashby");

        // Resumed: the conversation's name comes back by its root...
        let t = feeds.join("t.jsonl");
        std::fs::write(
            &t,
            "{\"type\":\"user\",\"uuid\":\"r1\",\"parentUuid\":null}\n",
        )
        .unwrap();
        assert_eq!(
            find_among(&feeds, "again", Some(&t), || live_of(&["again"])).as_deref(),
            Some("Rotem Ashby")
        );
        // ...unless another live session took that first name meanwhile:
        // then none, and the session is drawn a new one.
        record_held(&feeds, "other", "Rotem Tennyson", None, || panic!(), &[]).unwrap();
        let taken = || live_of(&["other", "later"]);
        assert_eq!(find_among(&feeds, "later", Some(&t), taken), None);
        assert_eq!(read(&feeds, "later"), None);
        let _ = std::fs::remove_dir_all(&feeds);
    }

    /// Every first name but `free` held by a live session in `feeds`.
    fn all_held_but(feeds: &Path, free: &[&str]) -> Live {
        let ids: Vec<String> = FIRST_NAMES
            .iter()
            .filter(|f| !free.contains(f))
            .enumerate()
            .map(|(i, first)| {
                let id = format!("h{i}");
                record_held(
                    feeds,
                    &id,
                    &format!("{first} Ashby"),
                    None,
                    || panic!(),
                    &[],
                )
                .unwrap();
                id
            })
            .collect();
        live_of(&ids.iter().map(String::as_str).collect::<Vec<_>>())
    }

    #[test]
    fn two_sessions_drawing_at_once_never_share_a_first_name() {
        // giverny#433: both read the holders before either recorded. Two
        // first names left: drawn at once, one each, every time; with the
        // two racers live, and with neither in the registry yet (their
        // names fresh).
        let free = ["Rotem", "Tamsin"];
        for registered in [true, false] {
            let feeds = scratch(&format!("race-{registered}"));
            let live = all_held_but(&feeds, &free);
            for round in 0..25 {
                let (a, b) = (format!("a{round}"), format!("b{round}"));
                let mut live = live.clone();
                if registered {
                    live.insert(a.clone(), PathBuf::new());
                    live.insert(b.clone(), PathBuf::new());
                }
                let gate = std::sync::Barrier::new(2);
                let (na, nb) = std::thread::scope(|s| {
                    let draw = |id: &str| {
                        let (feeds, live, gate) = (&feeds, &live, &gate);
                        let id = id.to_string();
                        s.spawn(move || {
                            gate.wait();
                            draw_and_record_among(feeds, &id, None, "Giverny", live).unwrap()
                        })
                    };
                    let (ta, tb) = (draw(&a), draw(&b));
                    (ta.join().unwrap(), tb.join().unwrap())
                });
                assert_ne!(first_of(&na), first_of(&nb), "round {round}: {na} / {nb}");
                for n in [&na, &nb] {
                    assert!(free.contains(&n.split_once(' ').unwrap().0), "{n}");
                }
                // Both done with: their first names free for the next round.
                for id in [&a, &b] {
                    let _ = std::fs::remove_file(session_file(&feeds, id).unwrap());
                }
            }
            let _ = std::fs::remove_dir_all(&feeds);
        }
    }

    #[test]
    fn a_pre_room_doc_draws_its_room_once_however_many_look_at_once() {
        // giverny#441: a document from before rooms, looked at by several
        // at once while its root is stamped: one room, on both files.
        let feeds = scratch("room-race");
        let t = feeds.join("t.jsonl");
        std::fs::create_dir_all(&feeds).unwrap();
        std::fs::write(
            &t,
            "{\"type\":\"user\",\"uuid\":\"root-1\",\"parentUuid\":null}\n",
        )
        .unwrap();
        for round in 0..25 {
            let id = format!("s{round}");
            let file = session_file(&feeds, &id).unwrap();
            write_doc(&file, &json!({"session": id, "name": "Rotem Ashby"})).unwrap();
            let _ = std::fs::remove_file(root_file(&feeds, "root-1").unwrap());
            let gate = std::sync::Barrier::new(5);
            let rooms: Vec<String> = std::thread::scope(|s| {
                let (feeds, id, gate, t) = (&feeds, &id, &gate, &t);
                let stamp = s.spawn(move || {
                    gate.wait();
                    keep_root(feeds, id, Some(t));
                });
                let lookers: Vec<_> = (0..4)
                    .map(|_| {
                        s.spawn(move || {
                            gate.wait();
                            room(feeds, id).unwrap()
                        })
                    })
                    .collect();
                stamp.join().unwrap();
                lookers.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let doc = read_doc(&file).unwrap();
            let kept = doc_room(&doc).expect("kept");
            assert!(
                rooms.iter().all(|r| *r == kept),
                "round {round}: {rooms:?} / {kept}"
            );
            assert_eq!(doc.get("root").and_then(Value::as_str), Some("root-1"));
            assert_eq!(room(&feeds, &id), Some(kept));
        }
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_session_named_only_on_an_old_feed_gets_a_place() {
        // giverny#440: named before names had a home; its tab is titled
        // after it the first time a place is asked for, and the same after.
        let feeds = scratch("feed-place");
        set_on(&feeds, true).unwrap();
        let feed = crate::feed::feed_path(&feeds, "old");
        std::fs::create_dir_all(feed.parent().unwrap()).unwrap();
        std::fs::write(
            &feed,
            json!({"version": 1, "session": "old", "name": "Juniper Belfry", "rows": []})
                .to_string(),
        )
        .unwrap();
        let first = place(&feeds, "old").expect("a place");
        assert!(first.starts_with("Juniper's "), "{first}");
        assert_eq!(read(&feeds, "old").as_deref(), Some("Juniper Belfry"));
        for _ in 0..3 {
            assert_eq!(place(&feeds, "old"), Some(first.clone()));
        }
        assert_eq!(
            shown(&feeds, "old", None).as_deref(),
            Some("Juniper Belfry")
        );
        assert_eq!(place(&feeds, "nobody"), None, "no feed, no place");
        let _ = std::fs::remove_dir_all(&feeds);
    }
}
