//! Session names (giverny#343): with Settings → Management panel → "ask
//! sessions to name themselves" on, every Claude session in a Giverny tab is
//! named at its start, before its first turn (giverny#401): its
//! `SessionStart` hook draws a full name, first name and surname both drawn
//! here ([`draw_name`], giverny#364: sessions left to pick a first name kept
//! picking the same few), records it, and tells the session. The status line
//! shows it beside the model from the start: `Opus 5.5 (Tamsin Camden)`.
//!
//! A name is drawn once: `giverny-manage name` prints the one recorded. A
//! session may
//! still give its own: a first name alone gets a drawn surname, a full name
//! passed in (a revived session keeping its old one) is kept as given, and
//! naming again with the same first name keeps the surname already drawn, so
//! nobody is renamed halfway through.
//!
//! **No two live sessions share a first name** (giverny#425): Ita tells his
//! sessions apart by it. A session is live while its claude runs, by Claude
//! Code's own registry ([`live_sessions`]), so a first name comes free again
//! once its session ends and the pool never fills up over months. A drawn
//! first name is drawn from the pool less the ones live sessions hold
//! ([`draw_name`]); one given by hand that another live session holds is
//! refused, naming the holder ([`record`]); and a resumed session takes its
//! conversation's old name back only while nobody live took that first name
//! meanwhile ([`find`]), else it is drawn a new one. The same session, or
//! another id of the same conversation, re-taking its own name is fine. The
//! full name stays unique against every name ever recorded.
//!
//! **Where a name lives.** Beside the feeds, never in one: a session that
//! only names itself is no manager, and a feed file would make it one.
//! `<feeds>/names/<session>.json` holds `{session, name, root}`, and
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

/// Giverny's own surnames, drawn for every session that gives only a first
/// name or none.
pub const SURNAMES: &[&str] = &[
    "Abernethy",
    "Aldridge",
    "Amberley",
    "Appleyard",
    "Arkwright",
    "Ashby",
    "Atherton",
    "Bagshaw",
    "Bainbridge",
    "Barrowclough",
    "Beaumont",
    "Birchall",
    "Blakemore",
    "Bosworth",
    "Bramhall",
    "Brindley",
    "Brockhurst",
    "Burnside",
    "Cardew",
    "Carlisle",
    "Chadwick",
    "Chalcott",
    "Cholmondeley",
    "Clayborne",
    "Coverdale",
    "Crabtree",
    "Cranwell",
    "Dalloway",
    "Dashwood",
    "Dewhurst",
    "Draycott",
    "Dunstan",
    "Edgerton",
    "Elsworth",
    "Esterbrook",
    "Fairfax",
    "Fanshawe",
    "Fernley",
    "Fothergill",
    "Foxcroft",
    "Gainsford",
    "Garside",
    "Glenister",
    "Goodenough",
    "Gresham",
    "Hadley",
    "Halliwell",
    "Hanbury",
    "Harcourt",
    "Heathcote",
    "Hepworth",
    "Highsmith",
    "Holbrook",
    "Huntley",
    "Ingleby",
    "Inchbald",
    "Jarrold",
    "Keswick",
    "Kettleby",
    "Kilbride",
    "Lambourne",
    "Langley",
    "Lavenham",
    "Lindqvist",
    "Lowther",
    "Maplethorpe",
    "Marchbank",
    "Merriweather",
    "Middleton",
    "Moorcroft",
    "Morland",
    "Nettleford",
    "Newbold",
    "Northam",
    "Oldcastle",
    "Ormerod",
    "Oxley",
    "Padgett",
    "Partridge",
    "Penrose",
    "Pinkerton",
    "Ponsonby",
    "Quennell",
    "Radley",
    "Ramsbottom",
    "Ravenscroft",
    "Redfern",
    "Rowntree",
    "Rushworth",
    "Saltonstall",
    "Seddon",
    "Shackleton",
    "Sherwood",
    "Slingsby",
    "Southwell",
    "Spalding",
    "Stainforth",
    "Stirling",
    "Swaffham",
    "Tattersall",
    "Tennyson",
    "Thackeray",
    "Thirlwell",
    "Tolland",
    "Trevelyan",
    "Tunstall",
    "Upcott",
    "Verity",
    "Wainwright",
    "Walbrook",
    "Warburton",
    "Westbrook",
    "Whitcombe",
    "Willoughby",
    "Winterbourne",
    "Woodhouse",
    "Wrexham",
    "Yardley",
    "Youngblood",
];

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

fn write_doc(file: &Path, doc: &Value) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{doc}\n"))?;
    std::fs::rename(&tmp, file)
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
    let name = name_in(&root_file(feeds, &root)?)?;
    if held_by(&holders(feeds, &live(), session, Some(&root)), &name).is_some() {
        return None;
    }
    if let Some(file) = session_file(feeds, session) {
        let _ = write_doc(
            &file,
            &json!({"session": session, "name": name, "root": root}),
        );
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
    let mut doc = read_doc(&file)?;
    let name = doc_name(&doc)?;
    if doc.get("root").and_then(Value::as_str).is_none()
        && let Some(root) = transcript.and_then(crate::subagents::conversation_root)
    {
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
/// surname `draw()` gives.
pub fn full_name(
    given: &str,
    previous: Option<&str>,
    draw: impl FnOnce() -> String,
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
                None => format!("{first} {}", draw()),
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

/// A surname from [`SURNAMES`], at random.
pub fn draw_surname() -> String {
    pick(SURNAMES)
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

/// How many times [`draw_name`] draws again past a name already taken.
const REDRAWS: usize = 16;

/// A full name for a session that has none (giverny#364): a first name no
/// session in `held` holds (giverny#425: drawn from the pool less theirs,
/// not hoped for), and a surname, drawn again while the pair is one already
/// recorded under `feeds`. Past [`REDRAWS`] the last draw stands: two
/// sessions may share a full name, they just rarely do. Every first name
/// held (hundreds of live sessions) draws from the whole pool.
pub fn draw_name(feeds: &Path, held: &[Holder]) -> String {
    let taken = recorded(feeds);
    let free: Vec<&str> = FIRST_NAMES
        .iter()
        .copied()
        .filter(|f| held_by(held, f).is_none())
        .collect();
    let pool: &[&str] = if free.is_empty() { FIRST_NAMES } else { &free };
    untaken(&taken, || format!("{} {}", pick(pool), draw_surname()))
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

/// The first of `draw`'s names not in `taken`, or its last after
/// [`REDRAWS`] more.
fn untaken(taken: &std::collections::HashSet<String>, mut draw: impl FnMut() -> String) -> String {
    let mut name = draw();
    for _ in 0..REDRAWS {
        if !taken.contains(&name) {
            break;
        }
        name = draw();
    }
    name
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
    draw: impl FnOnce() -> String,
) -> Result<(String, Option<String>), String> {
    record_among(feeds, session, given, root, draw, &live_sessions(None))
}

/// [`record`] against the `live` sessions given.
pub fn record_among(
    feeds: &Path,
    session: &str,
    given: &str,
    root: Option<&str>,
    draw: impl FnOnce() -> String,
    live: &Live,
) -> Result<(String, Option<String>), String> {
    record_held(
        feeds,
        session,
        given,
        root,
        draw,
        &holders(feeds, live, session, root),
    )
}

/// [`record`], refused when a session in `held` holds the first name.
fn record_held(
    feeds: &Path,
    session: &str,
    given: &str,
    root: Option<&str>,
    draw: impl FnOnce() -> String,
    held: &[Holder],
) -> Result<(String, Option<String>), String> {
    let file =
        session_file(feeds, session).ok_or_else(|| format!("not a session id: {session:?}"))?;
    let previous = name_in(&file).or_else(|| name_in(&root_file(feeds, root?)?));
    let name = full_name(given, previous.as_deref(), draw)?;
    if let Some(h) = held_by(held, &name) {
        let first = name.split_whitespace().next().unwrap_or(&name);
        return Err(format!(
            "{first} is taken: {h} holds it, and no two live sessions share a first name; \
             give another, or run `giverny-manage name` alone to be drawn one"
        ));
    }
    let mut doc = json!({"session": session, "name": name});
    if let Some(r) = root {
        doc["root"] = json!(r);
    }
    write_doc(&file, &doc).map_err(|e| format!("{}: {e}", file.display()))?;
    if let Some(rf) = root.and_then(|r| root_file(feeds, r)) {
        write_doc(&rf, &doc).map_err(|e| format!("{}: {e}", rf.display()))?;
    }
    Ok((name, previous))
}

/// Draw `session` a full name ([`draw_name`]) and record it, under its
/// conversation's root when its `transcript` has one: what a session with
/// no name gets, from `giverny-manage name` or its start (giverny#401).
pub fn draw_and_record(
    feeds: &Path,
    session: &str,
    transcript: Option<&Path>,
) -> Result<String, String> {
    draw_and_record_among(feeds, session, transcript, &live_sessions(None))
}

/// [`draw_and_record`] against the `live` sessions given: the first name
/// drawn is none of theirs.
pub fn draw_and_record_among(
    feeds: &Path,
    session: &str,
    transcript: Option<&Path>,
    live: &Live,
) -> Result<String, String> {
    let root = transcript.and_then(crate::subagents::conversation_root);
    let held = holders(feeds, live, session, root.as_deref());
    // A full name: `record` keeps it as given and draws nothing more. Drawn
    // clear of `held` already, so checked against nobody: with every first
    // name held, the draw from the whole pool still stands.
    record_held(
        feeds,
        session,
        &draw_name(feeds, &held),
        root.as_deref(),
        draw_surname,
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
/// ([`draw_and_record`], giverny#401), so the session has its name before
/// its first turn. Marked as told, so it is not told again ([`ASKED`]).
fn tell(payload: &Value, feeds: &Path, session: &str) -> String {
    let transcript = transcript_of(payload);
    let name = find(feeds, session, transcript.as_deref())
        .or_else(|| on_feed(feeds, session))
        .or_else(|| {
            draw_and_record(feeds, session, transcript.as_deref())
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
        assert!(SURNAMES.iter().all(|s| is_word(s)));
        assert!(SURNAMES.contains(&draw_surname().as_str()));
        let _ = std::fs::remove_dir_all(&feeds);
    }

    #[test]
    fn a_drawn_name_is_a_first_name_and_a_surname_not_already_taken() {
        // giverny#364: Giverny draws the whole name from its own lists.
        assert!(FIRST_NAMES.len() >= 300, "a few hundred");
        assert!(FIRST_NAMES.iter().all(|s| is_word(s) && s.is_ascii()));
        let unique: std::collections::HashSet<_> = FIRST_NAMES.iter().collect();
        assert_eq!(unique.len(), FIRST_NAMES.len(), "no first name twice");
        assert!(FIRST_NAMES.contains(&draw_first_name().as_str()));

        let feeds = scratch("drawn");
        let drawn = draw_name(&feeds, &[]);
        let (first, last) = drawn.split_once(' ').unwrap();
        assert!(FIRST_NAMES.contains(&first), "{drawn}");
        assert!(SURNAMES.contains(&last), "{drawn}");
        // Recorded as drawn: a full name, kept as given.
        let (n, _) = record(&feeds, "s1", &drawn, None, || panic!("no draw")).unwrap();
        assert_eq!(n, drawn);
        assert!(recorded(&feeds).contains(&drawn));

        // A pair already recorded is drawn again; past the redraws the last
        // stands.
        let taken = recorded(&feeds);
        let mut draws = [drawn.clone(), "Tamsin Camden".into()].into_iter();
        assert_eq!(untaken(&taken, || draws.next().unwrap()), "Tamsin Camden");
        assert_eq!(untaken(&taken, || drawn.clone()), drawn);
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
        assert!(FIRST_NAMES.contains(&first) && SURNAMES.contains(&last));
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
            let name = draw_name(&feeds, &holders(&feeds, &live, "me", None));
            assert_eq!(name.split_once(' ').unwrap().0, *last, "{name}");
        }
        let name = draw_and_record_among(&feeds, "me", None, &live).unwrap();
        assert!(name.starts_with(&format!("{last} ")), "{name}");
        // Its session ended: h0's first name is free again (two now, the
        // last being taken by "me" is not live).
        live.remove("h0");
        live.insert("me".into(), PathBuf::new());
        let name = draw_and_record_among(&feeds, "you", None, &live).unwrap();
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
}
