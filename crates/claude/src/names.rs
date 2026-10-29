//! Session names: with `claude.name_sessions` on, every Claude session in a
//! Giverny tab is given a name at its start, before its first turn, and the
//! status line shows it beside the model: `Opus 4.5 (Rotem Giverny)`. With
//! several sessions at work, a name is what tells them apart, and what the
//! session can sign its work with.
//!
//! **A name is a first name and a surname.** The first name is drawn from
//! [`FIRST_NAMES`], never one a live session holds ([`draw_name`]): a
//! session is live while its claude runs, by Claude Code's own registry
//! ([`live_sessions`]), so a first name comes free again when its session
//! ends. The surname says where the session was born ([`surname`]): the rail
//! category its tab sits in, else its repo. It is fixed at birth: recorded
//! with the first name and never recomputed, so moving or renaming the tab
//! later renames nobody.
//!
//! **Where a name lives.** `<base>/names/<session>.json` holds
//! `{session, name, root}`, and `<base>/names/roots/<root>.json` the same,
//! keyed by the conversation's root ([`conversation_root`]): a resumed
//! session has a new id but the same root, so it finds its name there
//! ([`find`]). A session named at its start has no root yet (its transcript
//! holds no turn), so its next prompt and the status line stamp one in once
//! the transcript has it ([`keep_root`]). `<base>/names/on` exists while the
//! setting is on: the app writes and removes it ([`set_on`]), and the hooks
//! and the status line read it, so turning it on reaches sessions already
//! running.
//!
//! **When a session is named.** By the `SessionStart` hook ([`start_context`]),
//! which tells the session its name. A session that started with the setting
//! off is named on its next prompt instead ([`first_prompt`]).
//! `<base>/names/asked/<session>` marks the sessions told, so each is told
//! once per start.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The names directory under Giverny's config directory.
pub const DIR: &str = "names";
/// The setting, as a file in [`DIR`]: present while sessions are named and
/// their names shown.
const ON: &str = "on";
/// Under [`DIR`], an empty file per session already told its name, so the
/// prompt hook ([`first_prompt`]) never tells it twice.
const ASKED: &str = "asked";
/// Under [`DIR`], the lock every draw and every rewrite of a names document
/// is made under ([`lock`]), so two sessions starting at once never share a
/// first name.
const LOCK: &str = ".lock";
/// How long a names document just written counts its session as live
/// though Claude Code's registry does not list it yet: a session drawing in
/// the instant another starts still sees that one's name.
const FRESH: std::time::Duration = std::time::Duration::from_secs(30);
/// Lines of a transcript's head its conversation's root can hide in.
const ROOT_SCAN: usize = 500;

/// The first names a session is drawn from. Personal names of many origins,
/// short and easy to say; none a famous single-name figure, a tool or
/// language (`Ruby`, `Zig`), or a word that would read as part of a log line
/// (`May`, `Max`, `Dev`).
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

/// The environment variable a Giverny tab's shell carries the name of the
/// rail category the tab sits in when its shell starts: a session born there
/// takes it as its surname ([`surname`]) when the saved workspace cannot say
/// where the tab sits now.
pub const CATEGORY_ENV: &str = "GIVERNY_CATEGORY";

/// The environment variable Claude Code gives the sessions its background
/// daemon hosts. Such a session inherited the daemon's environment, so a
/// `GIVERNY_TAB_ID` in it names whatever tab started the daemon, not one it
/// runs in.
const JOB_DIR_ENV: &str = "CLAUDE_JOB_DIR";

/// Does a hook with this environment run for a Giverny tab's own session:
/// one with a tab id, and not a background job's?
pub fn in_tab_in(get: &dyn Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| get(name).is_some_and(|v| !v.trim().is_empty());
    set("GIVERNY_TAB_ID") && !set(JOB_DIR_ENV)
}

/// [`in_tab_in`] for this process.
pub fn in_tab() -> bool {
    in_tab_in(&|name| std::env::var(name).ok())
}

/// The surname a session is born with: the name of the rail category its
/// tab sits in, first letter capitalised (`giverny` → `Giverny`, `ToLaD`
/// stays); with no category, the repo it runs in (`cwd`'s git root, a
/// worktree counting as its main checkout, else `cwd`'s own name). The
/// tab's category in the workspace the app saved under `base` comes first,
/// [`CATEGORY_ENV`] only when the tab is not found there (a tab moved since
/// its shell started sits where the workspace says).
pub fn surname(base: &Path, cwd: Option<&Path>) -> String {
    surname_in(&|name| std::env::var(name).ok(), base, cwd)
}

/// [`surname`], reading the environment through `get`.
pub fn surname_in(get: &dyn Fn(&str) -> Option<String>, base: &Path, cwd: Option<&Path>) -> String {
    let var = |name: &str| {
        get(name)
            .filter(|v| !v.trim().is_empty())
            .filter(|_| get(JOB_DIR_ENV).is_none_or(|j| j.trim().is_empty()))
    };
    var("GIVERNY_TAB_ID")
        .and_then(|tab| tab_category(&base.join("state").join("tabs.json"), &tab))
        .or_else(|| var(CATEGORY_ENV))
        .and_then(|c| as_surname(&c))
        .or_else(|| {
            let cwd = cwd
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())?;
            as_surname(&repo_of(&cwd)?)
        })
        .unwrap_or_else(|| "Giverny".to_string())
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

/// The repo `cwd` is in, by name: its git root, a worktree counting as its
/// main checkout; else `cwd`'s own name.
fn repo_of(cwd: &Path) -> Option<String> {
    for dir in cwd.ancestors() {
        let git = dir.join(".git");
        if git.is_dir() {
            return name_of(dir);
        }
        if git.is_file() {
            // A worktree: `gitdir: <main>/.git/worktrees/<name>`.
            let main = std::fs::read_to_string(&git).ok().and_then(|s| {
                let gd = s.trim().strip_prefix("gitdir:")?.trim().to_string();
                let cut = gd.find("/.git/")?;
                Some(PathBuf::from(&gd[..cut]))
            });
            return main.as_deref().and_then(name_of).or_else(|| name_of(dir));
        }
    }
    name_of(cwd)
}

fn name_of(dir: &Path) -> Option<String> {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
}

/// `name` as a surname, kept readable rather than refused. Letters, digits
/// and apostrophes stay; spaces and any other character become one hyphen
/// (`lead machine` → `Lead-machine`); what does not start with a letter
/// loses its lead (`3D prints` → `D-prints`); the first letter is
/// capitalised, the rest kept as written. `None` when no letter is left.
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

/// `<base>/names`.
pub fn dir(base: &Path) -> PathBuf {
    base.join(DIR)
}

/// Are sessions named, and their names shown? A `stat`.
pub fn is_on(base: &Path) -> bool {
    dir(base).join(ON).is_file()
}

/// Follow the setting: write or remove `<base>/names/on`. Returns whether
/// anything changed.
pub fn set_on(base: &Path, on: bool) -> std::io::Result<bool> {
    let flag = dir(base).join(ON);
    if on == flag.is_file() {
        return Ok(false);
    }
    if on {
        std::fs::create_dir_all(dir(base))?;
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

fn session_file(base: &Path, session: &str) -> Option<PathBuf> {
    safe(session).then(|| dir(base).join(format!("{session}.json")))
}

fn asked_file(base: &Path, session: &str) -> Option<PathBuf> {
    safe(session).then(|| dir(base).join(ASKED).join(session))
}

fn root_file(base: &Path, root: &str) -> Option<PathBuf> {
    safe(root).then(|| dir(base).join("roots").join(format!("{root}.json")))
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

fn write_doc(file: &Path, doc: &Value) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{doc}\n"))?;
    std::fs::rename(&tmp, file)
}

/// The names lock: held, reading what others hold and writing a names
/// document is one step, so two sessions starting at once draw in turn.
/// Released when dropped. Best effort: where the file system takes no lock,
/// the step runs unlocked.
#[must_use]
struct Lock(#[allow(dead_code)] Option<std::fs::File>);

fn lock(base: &Path) -> Lock {
    let _ = std::fs::create_dir_all(dir(base));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir(base).join(LOCK))
        .ok()
        .filter(|f| f.lock().is_ok());
    Lock(file)
}

/// The conversation a transcript holds: the `uuid` of its first parentless
/// user or assistant turn. Claude Code copies a conversation's records
/// forward when it gives a resumed session a new id, so this stays the same
/// where the session id does not; `/clear` starts a new one. `None` for a
/// transcript with no turn yet.
pub fn conversation_root(transcript: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(transcript).ok()?;
    for line in std::io::BufReader::new(file).lines().take(ROOT_SCAN) {
        let Ok(line) = line else { break };
        if !line.contains("\"uuid\"") || !line.contains("\"parentUuid\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if !matches!(
            v.get("type").and_then(Value::as_str),
            Some("user" | "assistant")
        ) {
            continue;
        }
        let parent = v.get("parentUuid").and_then(Value::as_str);
        if parent.is_none_or(str::is_empty)
            && let Some(uuid) = v.get("uuid").and_then(Value::as_str)
        {
            return Some(uuid.to_string());
        }
    }
    None
}

/// The name recorded for `session` itself: one small file read.
pub fn read(base: &Path, session: &str) -> Option<String> {
    doc_name(&read_doc(&session_file(base, session)?)?)
}

/// `session`'s name: its own, else the one recorded for the conversation its
/// transcript holds (a resumed session's new id), which is then recorded
/// under the new id too, so the next look is one read again. Not the
/// conversation's name when another live session took its first name while
/// this one was gone: then none, and the session is drawn a new one.
pub fn find(base: &Path, session: &str, transcript: Option<&Path>) -> Option<String> {
    find_among(base, session, transcript, live_sessions)
}

/// [`find`], asking `live` for the live sessions only when it takes a
/// conversation's name.
fn find_among(
    base: &Path,
    session: &str,
    transcript: Option<&Path>,
    live: impl FnOnce() -> Live,
) -> Option<String> {
    if let Some(name) = read(base, session) {
        return Some(name);
    }
    let root = conversation_root(transcript?)?;
    let name = doc_name(&read_doc(&root_file(base, &root)?)?)?;
    let live = live();
    // Checked and taken in one step.
    let _lock = lock(base);
    if let Some(name) = read(base, session) {
        return Some(name);
    }
    if held_by(&holders(base, &live, session, Some(&root)), &name) {
        return None;
    }
    if let Some(file) = session_file(base, session) {
        let _ = write_doc(
            &file,
            &json!({"session": session, "name": name, "root": root}),
        );
    }
    Some(name)
}

/// Stamp `session`'s conversation root into its name once its `transcript`
/// has one: a session named at its start has no turn yet, so no root, and
/// without one a resume would not find the name ([`find`]). Writes the
/// root's file too. Returns the name recorded, if any; a session already
/// stamped costs one read.
pub fn keep_root(base: &Path, session: &str, transcript: Option<&Path>) -> Option<String> {
    let file = session_file(base, session)?;
    let doc = read_doc(&file)?;
    let name = doc_name(&doc)?;
    let unstamped = |doc: &Value| doc.get("root").and_then(Value::as_str).is_none();
    if unstamped(&doc)
        && let Some(root) = transcript.and_then(conversation_root)
    {
        let _lock = lock(base);
        let Some(mut doc) = read_doc(&file).filter(unstamped) else {
            return Some(name);
        };
        doc["root"] = json!(root);
        let _ = write_doc(&file, &doc);
        if let Some(rf) = root_file(base, &root) {
            let _ = write_doc(&rf, &doc);
        }
    }
    Some(name)
}

/// The name the status line shows for `session`: none while the setting is
/// off; else its name ([`find`], its root stamped in once known,
/// [`keep_root`]).
pub fn shown(base: &Path, session: &str, transcript: Option<&Path>) -> Option<String> {
    if !is_on(base) {
        return None;
    }
    keep_root(base, session, transcript).or_else(|| find(base, session, transcript))
}

/// One word of a name: a letter, then letters, digits, hyphens and
/// apostrophes; 40 characters at most.
#[cfg(test)]
fn is_word(w: &str) -> bool {
    let mut chars = w.chars();
    chars.next().is_some_and(char::is_alphabetic)
        && chars.all(|c| c.is_alphanumeric() || c == '-' || c == '\'')
        && w.chars().count() <= 40
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

/// The ids of the sessions live now.
pub type Live = HashSet<String>;

/// The sessions whose claude runs now: Claude Code's own registry
/// (`sessions/<pid>.json`, pid-checked, [`crate::registry::scan`]) in this
/// session's account and every other one found on the machine. A session
/// gone is gone from it, whatever its files say.
pub fn live_sessions() -> Live {
    let own = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    let mut seen = HashSet::new();
    let dirs: Vec<PathBuf> = own
        .into_iter()
        .chain(crate::profiles::ambient_dirs())
        .filter(|d| seen.insert(d.canonicalize().unwrap_or_else(|_| d.clone())))
        .collect();
    crate::registry::scan(dirs)
        .into_iter()
        .map(|s| s.entry.session_id)
        .collect()
}

/// The names every `live` session but `session` itself holds. A session of
/// the same conversation (`root`: a resume's other id) is `session` itself
/// under another id, so it holds nothing against it.
fn holders(base: &Path, live: &Live, session: &str, root: Option<&str>) -> Vec<String> {
    live.iter()
        .filter(|id| id.as_str() != session)
        .filter_map(|id| {
            let doc = read_doc(&session_file(base, id)?)?;
            let theirs = doc.get("root").and_then(Value::as_str);
            if root.is_some() && theirs == root {
                return None;
            }
            doc_name(&doc)
        })
        .collect()
}

/// The names of the sessions not in `live` whose names document was written
/// in the last `within`: just named, and live by now though Claude Code's
/// registry may not list them yet. `session` itself and another id of its
/// conversation (`root`) hold nothing against it.
fn fresh(
    base: &Path,
    live: &Live,
    session: &str,
    root: Option<&str>,
    within: std::time::Duration,
) -> Vec<String> {
    let now = std::time::SystemTime::now();
    let Ok(entries) = std::fs::read_dir(dir(base)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_str()?.to_string();
            if path.extension()? != "json" || id == session || live.contains(&id) {
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
            doc_name(&doc)
        })
        .collect()
}

/// A name's first name, for comparing: case aside.
fn first_of(name: &str) -> Option<String> {
    name.split_whitespace().next().map(str::to_lowercase)
}

/// Does a name in `held` have `name`'s first name?
fn held_by(held: &[String], name: &str) -> bool {
    first_of(name).is_some_and(|first| {
        held.iter()
            .any(|h| first_of(h).as_deref() == Some(first.as_str()))
    })
}

/// A full name: a first name none in `held` has (every first name held, the
/// whole pool) and `surname`. Two sessions may share a full name over time
/// (a second Rotem Giverny once the first has ended); no two live ones
/// share a first name.
pub fn draw_name(held: &[String], surname: &str) -> String {
    let free: Vec<&str> = FIRST_NAMES
        .iter()
        .copied()
        .filter(|f| !held_by(held, f))
        .collect();
    let pool: &[&str] = if free.is_empty() { FIRST_NAMES } else { &free };
    format!("{} {surname}", pick(pool))
}

/// Draw `session` a name with `surname` ([`draw_name`]) and record it,
/// under its conversation's root too when its `transcript` has one. The
/// first name drawn is none a `live` session holds, nor one just given to a
/// session the registry does not list yet.
fn draw_and_record(
    base: &Path,
    session: &str,
    transcript: Option<&Path>,
    surname: &str,
    live: &Live,
) -> std::io::Result<String> {
    let file = session_file(base, session)
        .ok_or_else(|| std::io::Error::other(format!("not a session id: {session:?}")))?;
    let root = transcript.and_then(conversation_root);
    // Drawn and recorded in one step: a session drawing at the same instant
    // draws after, and sees this one's name.
    let _lock = lock(base);
    let mut held = holders(base, live, session, root.as_deref());
    held.extend(fresh(base, live, session, root.as_deref(), FRESH));
    let name = draw_name(&held, surname);
    let mut doc = json!({"session": session, "name": name});
    if let Some(r) = &root {
        doc["root"] = json!(r);
    }
    write_doc(&file, &doc)?;
    if let Some(rf) = root.as_deref().and_then(|r| root_file(base, r)) {
        write_doc(&rf, &doc)?;
    }
    Ok(name)
}

/// What a session is told of its name.
pub fn told(name: &str) -> String {
    format!("Your name in this session is {name}. Giverny shows it on the status line.")
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

/// Name `session` and say so ([`told`]): the name it has ([`find`]), else
/// one drawn and recorded right here with the surname of where it was born
/// ([`surname`]), so the session has its name before its first turn.
/// Marked as told, so it is not told again ([`ASKED`]). `None` only when no
/// name could be recorded.
fn tell(
    payload: &Value,
    base: &Path,
    session: &str,
    surname: impl FnOnce() -> String,
    live: impl Fn() -> Live,
) -> Option<String> {
    let transcript = transcript_of(payload);
    let name = find_among(base, session, transcript.as_deref(), &live).or_else(|| {
        draw_and_record(base, session, transcript.as_deref(), &surname(), &live()).ok()
    });
    if let Some(file) = asked_file(base, session) {
        let _ = std::fs::create_dir_all(dir(base).join(ASKED));
        let _ = std::fs::write(file, "");
    }
    name.as_deref().map(told)
}

/// What a session's start is told, naming it first ([`tell`]): only with
/// the setting `on`, only for a tab's own session (`in_tab`, [`in_tab`]),
/// never a subagent's. Every start tells again (a `/clear` is a new session
/// and draws a new name; a resume or a compaction keeps its name).
pub fn start_context(payload: &Value, base: &Path, on: bool, in_tab: bool) -> Option<String> {
    start_context_among(
        payload,
        base,
        on,
        in_tab,
        || surname(base, cwd_of(payload).as_deref()),
        live_sessions,
    )
}

fn start_context_among(
    payload: &Value,
    base: &Path,
    on: bool,
    in_tab: bool,
    surname: impl FnOnce() -> String,
    live: impl Fn() -> Live,
) -> Option<String> {
    if !on || !in_tab || payload.get("agent_id").is_some() {
        return None;
    }
    let session = session_of(payload)?;
    tell(payload, base, session, surname, live)
}

/// What a `UserPromptSubmit` is told when the session's start told nothing
/// (the setting was off then): its name, as its start would have. The same
/// gates as [`start_context`], plus once per session. A session told already
/// has its root stamped in instead, once its transcript has one
/// ([`keep_root`]).
pub fn first_prompt(payload: &Value, base: &Path, on: bool, in_tab: bool) -> Option<String> {
    first_prompt_among(
        payload,
        base,
        on,
        in_tab,
        || surname(base, cwd_of(payload).as_deref()),
        live_sessions,
    )
}

fn first_prompt_among(
    payload: &Value,
    base: &Path,
    on: bool,
    in_tab: bool,
    surname: impl FnOnce() -> String,
    live: impl Fn() -> Live,
) -> Option<String> {
    if !on || !in_tab || payload.get("agent_id").is_some() {
        return None;
    }
    let session = session_of(payload)?;
    if asked_file(base, session)?.exists() {
        keep_root(base, session, transcript_of(payload).as_deref());
        return None;
    }
    tell(payload, base, session, surname, live)
}

/// The hook reply that hands `context` to the session at `event`.
pub fn hook_reply(event: &str, context: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context
        }
    })
    .to_string()
}

/// What the hook for `event` replies to `payload`: the session's name at
/// `SessionStart` ([`start_context`]), or at a `UserPromptSubmit` when its
/// start said nothing ([`first_prompt`]). Nothing for any other event, nor
/// while the setting is off, which costs one `stat`.
pub fn reply(payload: &Value, base: &Path, in_tab: bool) -> Option<String> {
    let event = payload.get("hook_event_name").and_then(Value::as_str)?;
    if !matches!(event, "SessionStart" | "UserPromptSubmit") || !is_on(base) {
        return None;
    }
    let context = match event {
        "SessionStart" => start_context(payload, base, true, in_tab),
        _ => first_prompt(payload, base, true, in_tab),
    }?;
    Some(hook_reply(event, &context))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-names-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Every name recorded under a session id in `base`'s names directory.
    fn recorded(base: &Path) -> HashSet<String> {
        std::fs::read_dir(dir(base))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| doc_name(&read_doc(&p)?))
            .collect()
    }

    /// Record `name` for `session` (and its `root`) as a draw would have.
    fn give(base: &Path, session: &str, name: &str, root: Option<&str>) {
        let mut doc = json!({"session": session, "name": name});
        if let Some(r) = root {
            doc["root"] = json!(r);
            write_doc(&root_file(base, r).unwrap(), &doc).unwrap();
        }
        write_doc(&session_file(base, session).unwrap(), &doc).unwrap();
    }

    fn live_of(ids: &[&str]) -> Live {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn nobody() -> Live {
        Live::new()
    }

    /// An environment of `vars` alone.
    fn env_of<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn start(session: &str, extra: Value) -> Value {
        let mut v = json!({"session_id": session, "hook_event_name": "SessionStart",
                           "source": "startup"});
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        v
    }

    fn prompt(session: &str) -> Value {
        json!({"session_id": session, "hook_event_name": "UserPromptSubmit", "prompt": "hi"})
    }

    fn at_start(p: &Value, base: &Path, on: bool, in_tab: bool) -> Option<String> {
        start_context_among(p, base, on, in_tab, || "Giverny".into(), nobody)
    }

    fn on_prompt(p: &Value, base: &Path, on: bool, in_tab: bool) -> Option<String> {
        first_prompt_among(p, base, on, in_tab, || "Giverny".into(), nobody)
    }

    fn transcript_with_root(path: &Path, root: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!("{{\"type\":\"user\",\"uuid\":\"{root}\",\"parentUuid\":null}}\n"),
        )
        .unwrap();
    }

    #[test]
    fn a_drawn_name_is_a_first_name_and_the_sessions_surname() {
        assert!(FIRST_NAMES.len() >= 300, "a few hundred");
        assert!(FIRST_NAMES.iter().all(|s| is_word(s) && s.is_ascii()));
        let unique: HashSet<_> = FIRST_NAMES.iter().collect();
        assert_eq!(unique.len(), FIRST_NAMES.len(), "no first name twice");

        let drawn = draw_name(&[], "Giverny");
        let (first, last) = drawn.split_once(' ').unwrap();
        assert!(FIRST_NAMES.contains(&first), "{drawn}");
        assert_eq!(last, "Giverny");

        let base = scratch("drawn");
        let name = draw_and_record(&base, "s3", None, "ToLaD", &nobody()).unwrap();
        assert!(name.ends_with(" ToLaD"), "{name}");
        assert_eq!(read(&base, "s3"), Some(name));
        assert!(draw_and_record(&base, "../x", None, "ToLaD", &nobody()).is_err());
        let _ = std::fs::remove_dir_all(&base);
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
        // A repo, and a worktree of it.
        let repo = base.join("ToLaD");
        std::fs::create_dir_all(repo.join(".git/worktrees/w1")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let wt = base.join("worktrees/task-1");
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
        let at = |vars: &[(&str, &str)], cwd: &Path| surname_in(&env_of(vars), &base, Some(cwd));
        assert_eq!(at(&tab, &repo), "Giverny", "the category, not the repo");
        assert_eq!(at(&[(CATEGORY_ENV, "lead machine")], &repo), "Lead-machine");
        // No category: the repo, a worktree counting as its main checkout,
        // else the directory's own name.
        assert_eq!(at(&[], &repo.join("src")), "ToLaD");
        assert_eq!(at(&[], &wt), "ToLaD");
        assert_eq!(at(&[], &plain), "Scratch-dir");
        assert_eq!(at(&[(CATEGORY_ENV, " ")], &repo), "ToLaD", "blank is none");
        assert_eq!(at(&[(CATEGORY_ENV, "☕")], &repo), "ToLaD", "no letter");

        // The workspace the app saved says where the tab sits now.
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
        // The tab moved to inbar since its shell started in giverny.
        assert_eq!(at(&tab, &repo), "Inbar");
        // A tab the saved workspace does not know: the shell's own word.
        assert_eq!(
            at(
                &[(CATEGORY_ENV, "giverny"), ("GIVERNY_TAB_ID", "giverny-99")],
                &repo
            ),
            "Giverny"
        );
        // Inside a background job the tab's variables are some other tab's.
        let job = [
            ("CLAUDE_JOB_DIR", "/home/x/.claude/jobs/abc"),
            (CATEGORY_ENV, "giverny"),
            ("GIVERNY_TAB_ID", "giverny-12"),
        ];
        assert_eq!(at(&job, &repo), "ToLaD");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn only_a_tabs_own_session_is_in_a_tab() {
        assert!(in_tab_in(&env_of(&[("GIVERNY_TAB_ID", "giverny-1")])));
        assert!(!in_tab_in(&env_of(&[])));
        assert!(!in_tab_in(&env_of(&[("GIVERNY_TAB_ID", " ")])));
        assert!(!in_tab_in(&env_of(&[
            ("GIVERNY_TAB_ID", "giverny-1"),
            ("CLAUDE_JOB_DIR", "/home/x/.claude/jobs/abc"),
        ])));
    }

    #[test]
    fn the_setting_hides_every_name() {
        let base = scratch("switch");
        give(&base, "s1", "Tamsin Camden", None);
        assert!(!is_on(&base), "absent means off");
        assert_eq!(shown(&base, "s1", None), None);
        assert!(set_on(&base, true).unwrap());
        assert!(!set_on(&base, true).unwrap(), "a no-op the second time");
        assert_eq!(shown(&base, "s1", None).as_deref(), Some("Tamsin Camden"));
        assert!(set_on(&base, false).unwrap());
        assert_eq!(shown(&base, "s1", None), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_name_is_given_only_with_the_setting_on_and_never_to_a_subagent() {
        let base = scratch("ask");
        let p = start("s1", json!({}));
        assert_eq!(at_start(&p, &base, false, true), None, "setting off");
        assert_eq!(at_start(&p, &base, true, false), None, "not in a tab");
        let sub = start("s1", json!({"agent_id": "a1"}));
        assert_eq!(at_start(&sub, &base, true, true), None, "a subagent");
        assert!(recorded(&base).is_empty(), "nobody named yet");
        // A fresh start is named right there, before any turn.
        let told = at_start(&p, &base, true, true).unwrap();
        let name = read(&base, "s1").expect("recorded at the start");
        let (first, last) = name.split_once(' ').unwrap();
        assert!(FIRST_NAMES.contains(&first), "{name}");
        assert_eq!(last, "Giverny");
        assert!(
            told.starts_with(&format!("Your name in this session is {name}.")),
            "{told}"
        );
        assert!(!told.contains('\n'), "one line");
        // Started again (a compaction): the same name, nothing drawn.
        let again = start_context_among(&p, &base, true, true, || panic!("no draw"), nobody);
        assert_eq!(again.as_deref(), Some(told.as_str()));
        assert_eq!(recorded(&base).len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_hook_reply_carries_the_name_at_start_and_at_a_first_prompt() {
        let base = scratch("reply");
        let p = start("s1", json!({}));
        assert_eq!(reply(&p, &base, true), None, "setting off");
        set_on(&base, true).unwrap();
        assert_eq!(reply(&p, &base, false), None, "not in a tab");
        let out: Value = serde_json::from_str(&reply(&p, &base, true).unwrap()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "SessionStart");
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains(&read(&base, "s1").unwrap()), "{context}");
        // Told at its start, so its prompt says nothing.
        assert_eq!(reply(&prompt("s1"), &base, true), None);
        // Any other event is left alone.
        let stop = json!({"session_id": "s2", "hook_event_name": "Stop"});
        assert_eq!(reply(&stop, &base, true), None);
        assert_eq!(read(&base, "s2"), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_session_whose_start_named_nobody_is_named_on_its_next_prompt_once() {
        let base = scratch("first-prompt");
        let p = prompt("s1");
        assert_eq!(on_prompt(&p, &base, false, true), None, "setting off");
        assert_eq!(on_prompt(&p, &base, true, false), None, "not in a tab");
        let mut sub = prompt("s1");
        sub["agent_id"] = json!("a1");
        assert_eq!(on_prompt(&sub, &base, true, true), None, "a subagent");
        assert_eq!(read(&base, "s1"), None, "nobody named yet");
        // Named on the prompt, as its start would have.
        let told = on_prompt(&p, &base, true, true).unwrap();
        let name = read(&base, "s1").expect("recorded on the prompt");
        assert!(told.contains(&name), "{told}");
        assert_eq!(on_prompt(&p, &base, true, true), None, "told once");
        // A session its start named already is not told again, nor renamed.
        assert!(at_start(&start("s2", json!({})), &base, true, true).is_some());
        let s2 = read(&base, "s2").unwrap();
        assert_eq!(on_prompt(&prompt("s2"), &base, true, true), None);
        assert_eq!(read(&base, "s2"), Some(s2));
        // Off at its start, so neither named nor marked: on later, its next
        // prompt names it.
        assert_eq!(at_start(&start("s4", json!({})), &base, false, true), None);
        assert_eq!(read(&base, "s4"), None);
        assert!(on_prompt(&prompt("s4"), &base, true, true).is_some());
        assert!(read(&base, "s4").is_some());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_session_named_at_its_start_keeps_its_name_across_a_resume() {
        let base = scratch("start-resume");
        set_on(&base, true).unwrap();
        let t = base.join("projects/s1.jsonl");
        let payload = |session: &str, event: &str| {
            json!({"session_id": session, "hook_event_name": event,
                   "transcript_path": t.to_str().unwrap()})
        };
        // A fresh start: no transcript yet, so no root.
        at_start(&payload("s1", "SessionStart"), &base, true, true).unwrap();
        let name = read(&base, "s1").unwrap();
        assert!(read_doc(&session_file(&base, "s1").unwrap()).unwrap()["root"].is_null());
        // A prompt before the transcript has a turn: nothing to stamp.
        assert_eq!(
            on_prompt(&payload("s1", "UserPromptSubmit"), &base, true, true),
            None
        );
        assert!(!dir(&base).join("roots").exists());
        // The first turn lands; the next prompt stamps the root.
        transcript_with_root(&t, "root-1");
        on_prompt(&payload("s1", "UserPromptSubmit"), &base, true, true);
        assert_eq!(
            read_doc(&root_file(&base, "root-1").unwrap())
                .as_ref()
                .and_then(doc_name),
            Some(name.clone())
        );
        // Resumed under a new id, the same transcript's conversation: the
        // same name, nothing drawn.
        let told = start_context_among(
            &payload("s2", "SessionStart"),
            &base,
            true,
            true,
            || panic!("no draw"),
            nobody,
        )
        .unwrap();
        assert!(told.contains(&name), "{told}");
        assert_eq!(read(&base, "s2"), Some(name.clone()));

        // The status line stamps the root too, for a session not prompted
        // since.
        let u = base.join("projects/s3.jsonl");
        at_start(
            &json!({"session_id": "s3", "hook_event_name": "SessionStart",
                    "transcript_path": u.to_str().unwrap()}),
            &base,
            true,
            true,
        )
        .unwrap();
        let s3 = read(&base, "s3").unwrap();
        assert_eq!(shown(&base, "s3", Some(&u)), Some(s3.clone()));
        transcript_with_root(&u, "root-3");
        assert_eq!(shown(&base, "s3", Some(&u)), Some(s3.clone()));
        assert_eq!(
            read_doc(&root_file(&base, "root-3").unwrap())
                .as_ref()
                .and_then(doc_name),
            Some(s3)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_resume_does_not_take_back_a_first_name_a_live_session_took() {
        let base = scratch("resume-taken");
        let t = base.join("projects/new.jsonl");
        transcript_with_root(&t, "root-1");
        give(&base, "old", "Rotem Giverny", Some("root-1"));
        // While it was gone, another session took Rotem.
        give(&base, "other", "Rotem Inbar", None);
        let live = || live_of(&["other"]);
        assert_eq!(find_among(&base, "new", Some(&t), live), None);
        // Had that session ended, the name would come back with the
        // conversation.
        assert_eq!(
            find_among(&base, "elsewhen", Some(&t), nobody).as_deref(),
            Some("Rotem Giverny")
        );
        // It has not: the resume is drawn a new name, which the conversation
        // carries from then on.
        let p = json!({"session_id": "new", "hook_event_name": "SessionStart",
                       "transcript_path": t.to_str().unwrap()});
        let told = start_context_among(&p, &base, true, true, || "Giverny".into(), live).unwrap();
        let name = read(&base, "new").unwrap();
        assert!(!name.starts_with("Rotem "), "{name}");
        assert!(told.contains(&name), "{told}");
        assert_eq!(find_among(&base, "newer", Some(&t), nobody), Some(name));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_drawn_first_name_is_none_a_live_session_holds() {
        let base = scratch("unique");
        // Every first name but two is held by a live session.
        let free = ["Aarav", "Zuri"];
        let mut live = Live::new();
        for (i, f) in FIRST_NAMES.iter().filter(|f| !free.contains(f)).enumerate() {
            let id = format!("live-{i}");
            give(&base, &id, &format!("{f} Elsewhere"), None);
            live.insert(id);
        }
        for n in 0..20 {
            let name = draw_and_record(&base, &format!("s{n}"), None, "Giverny", &live).unwrap();
            let first = name.split_whitespace().next().unwrap();
            assert!(free.contains(&first), "{name}");
            std::fs::remove_file(session_file(&base, &format!("s{n}")).unwrap()).unwrap();
        }
        // A dead holder's first name is free again.
        assert!(!held_by(&holders(&base, &Live::new(), "s", None), "Rotem"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn two_sessions_drawing_at_once_never_share_a_first_name() {
        let base = scratch("race");
        let free = ["Aarav", "Zuri"];
        let mut held = Live::new();
        for (i, f) in FIRST_NAMES.iter().filter(|f| !free.contains(f)).enumerate() {
            let id = format!("live-{i}");
            give(&base, &id, &format!("{f} Elsewhere"), None);
            held.insert(id);
        }
        for round in 0..25 {
            let (a, b) = (format!("a{round}"), format!("b{round}"));
            let names: Vec<String> = std::thread::scope(|s| {
                let draw = |id: &str| {
                    let (base, held, id) = (&base, &held, id.to_string());
                    s.spawn(move || draw_and_record(base, &id, None, "Giverny", held).unwrap())
                };
                let (ta, tb) = (draw(&a), draw(&b));
                vec![ta.join().unwrap(), tb.join().unwrap()]
            });
            assert_ne!(first_of(&names[0]), first_of(&names[1]), "{names:?}");
            for id in [&a, &b] {
                std::fs::remove_file(session_file(&base, id).unwrap()).unwrap();
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_root_is_the_first_parentless_turn() {
        let base = scratch("root");
        let t = base.join("t.jsonl");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            &t,
            [
                r#"{"type":"summary","uuid":"x","parentUuid":null}"#,
                r#"{"type":"user","uuid":"side","parentUuid":null,"isSidechain":true}"#,
                r#"{"type":"user","uuid":"root-9","parentUuid":null}"#,
                r#"{"type":"assistant","uuid":"r2","parentUuid":"root-9"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        assert_eq!(conversation_root(&t).as_deref(), Some("root-9"));
        assert_eq!(conversation_root(&base.join("none.jsonl")), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
