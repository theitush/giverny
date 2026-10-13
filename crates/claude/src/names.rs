//! Session names (giverny#343): with Settings → Management panel → "ask
//! sessions to name themselves" on, every Claude session in a Giverny tab is
//! asked at its start to run `giverny manage name`, and Giverny gives it a
//! full name, first name and surname both drawn here ([`draw_name`],
//! giverny#364: sessions left to pick a first name kept picking the same
//! few). The status line shows it beside the model: `Opus 5.5 (Tamsin
//! Camden)`.
//!
//! A name is drawn once: asking again prints the one recorded. A session may
//! still give its own: a first name alone gets a drawn surname, a full name
//! passed in (a revived session keeping its old one) is kept as given, and
//! naming again with the same first name keeps the surname already drawn, so
//! nobody is renamed halfway through. Two sessions may share a first name;
//! the surnames tell them apart.
//!
//! **Where a name lives.** Beside the feeds, never in one: a session that
//! only names itself is no manager, and a feed file would make it one.
//! `<feeds>/names/<session>.json` holds `{session, name, root}`, and
//! `<feeds>/names/roots/<root>.json` the same, keyed by the conversation's
//! root ([`crate::continuation`]): a resumed session has a new id but the same
//! root, so the status line finds its name there and copies it under the new
//! id (giverny#336). `<feeds>/names/on` exists while the switch is on; the
//! app writes and removes it, and the status line and the session-start
//! hook read it.
//!
//! **When a session is asked** (giverny#356). Claude Code reads a plugin's
//! `hooks.json` once, when its process starts (and on `/reload-plugins`),
//! never on `/clear`, a compaction or a resume, measured on 2.1.296: a claude
//! started before the plugin gained a hook never runs it. So the plugin's
//! `SessionStart` hook is always registered and reads the switch itself
//! ([`session_start`]), and flipping it reaches every session started since
//! at its next start. A session whose process predates the hook, or that
//! started with the switch off, is asked on its next prompt instead, by the
//! `UserPromptSubmit` hook it does run ([`first_prompt`]). Either way a
//! session is asked once: `<feeds>/names/asked/<session>` marks the ones
//! told.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The names directory under the feed directory.
pub const DIR: &str = "names";
/// The switch, as a file in [`DIR`]: present while names are asked for and
/// shown.
const ON: &str = "on";
/// Under [`DIR`], an empty file per session already told [`ask`], so the
/// prompt hook's fallback ([`first_prompt`]) never asks twice.
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

/// Are sessions asked to name themselves, and their names shown? A `stat`.
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
    let doc: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
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
/// recorded under the new id too, so the next look is one read again.
pub fn find(feeds: &Path, session: &str, transcript: Option<&Path>) -> Option<String> {
    if let Some(name) = read(feeds, session) {
        return Some(name);
    }
    let root = crate::subagents::conversation_root(transcript?)?;
    let name = name_in(&root_file(feeds, &root)?)?;
    if let Some(file) = session_file(feeds, session) {
        let _ = write_doc(
            &file,
            &json!({"session": session, "name": name, "root": root}),
        );
    }
    Some(name)
}

/// The name the status line shows for `session`: none while the switch is
/// off; else its name ([`find`]), else the `name` on its manager feed (one
/// named before names had their own home, giverny#330).
pub fn shown(feeds: &Path, session: &str, transcript: Option<&Path>) -> Option<String> {
    if !is_on(feeds) {
        return None;
    }
    read(feeds, session)
        .or_else(|| {
            crate::feed::read(&crate::feed::feed_path(feeds, session))?
                .name
                .filter(|n| !n.trim().is_empty())
        })
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

/// How many times [`draw_name`] draws again past a name already taken.
const REDRAWS: usize = 16;

/// A full name for a session that has none (giverny#364): a first name and a
/// surname, both drawn, drawn again while the pair is one already recorded
/// under `feeds` (a look at the names directory, no registry). Past
/// [`REDRAWS`] the last draw stands: two sessions may share a name, they
/// just rarely do.
pub fn draw_name(feeds: &Path) -> String {
    let taken = recorded(feeds);
    untaken(&taken, || {
        format!("{} {}", draw_first_name(), draw_surname())
    })
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
/// it replaced.
pub fn record(
    feeds: &Path,
    session: &str,
    given: &str,
    root: Option<&str>,
    draw: impl FnOnce() -> String,
) -> Result<(String, Option<String>), String> {
    let file =
        session_file(feeds, session).ok_or_else(|| format!("not a session id: {session:?}"))?;
    let previous = name_in(&file).or_else(|| name_in(&root_file(feeds, root?)?));
    let name = full_name(given, previous.as_deref(), draw)?;
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

/// What a session in a Giverny tab is told at its start: its name, when it
/// has one already (a resume, a compaction), else to ask Giverny for one
/// (giverny#364: Giverny draws the whole name, nothing is picked).
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

/// What `session` is told ([`ask`], with the name [`find`] gives it), marked
/// as told so it is not asked again ([`ASKED`]).
fn tell(payload: &Value, feeds: &Path, session: &str) -> String {
    let transcript = payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let name = find(feeds, session, transcript.as_deref());
    if let Some(file) = asked_file(feeds, session) {
        let _ = std::fs::create_dir_all(dir(feeds).join(ASKED));
        let _ = std::fs::write(file, "");
    }
    ask(name.as_deref())
}

/// The `SessionStart` hook's reply: [`ask`] as `additionalContext`, only with
/// the switch `on`, only for a top-level session (`in_tab`: a Giverny tab's
/// own, not a background job's or a nested claude's), never a subagent's.
/// Every start asks again (a `/clear` is a new session; a resume or a
/// compaction is told its name), and is marked so the prompt hook stays
/// quiet ([`first_prompt`]).
pub fn session_start(payload: &Value, feeds: &Path, on: bool, in_tab: bool) -> Option<String> {
    if !on || !in_tab || payload.get("agent_id").is_some() {
        return None;
    }
    let session = session_of(payload)?;
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": tell(payload, feeds, session)
            }
        })
        .to_string(),
    )
}

/// The ask a `UserPromptSubmit` carries when the session's start did not:
/// its claude predates the plugin's `SessionStart` hook, or the switch was
/// off then (giverny#356). The same gates as [`session_start`], plus once per
/// session; `in_tab` is asked only for a session not yet told, as it walks
/// the process tree. Returns the context text, for the prompt hook's reply.
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
    if asked_file(feeds, session)?.exists() || !in_tab() {
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
        let drawn = draw_name(&feeds);
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
    fn the_ask_goes_out_only_with_the_switch_on_and_never_to_a_subagent() {
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
        let ask_out = context(session_start(&p, &feeds, true, true).unwrap());
        assert!(ask_out.contains("run `giverny-manage name`"), "{ask_out}");
        // Giverny gives the name: nothing to pick (giverny#364).
        assert!(
            !ask_out.contains("pick") && !ask_out.contains("<First>"),
            "{ask_out}"
        );
        assert!(!ask_out.contains('\n'), "one line");
        // Named already (a resume, a compaction): told its name instead.
        record(&feeds, "s1", "Tamsin Camden", None, || panic!()).unwrap();
        let told = context(session_start(&p, &feeds, true, true).unwrap());
        assert!(told.contains("Tamsin Camden"), "{told}");
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
        let asked = first_prompt(&p, &feeds, true, yes).unwrap();
        assert!(asked.contains("run `giverny-manage name`"), "{asked}");
        assert_eq!(
            first_prompt(&p, &feeds, true, || panic!("no walk once told")),
            None,
            "asked once"
        );
        // A session its start asked already is not asked again.
        let start = json!({"session_id": "s2", "hook_event_name": "SessionStart",
                           "source": "clear"});
        assert!(session_start(&start, &feeds, true, true).is_some());
        assert_eq!(first_prompt(&prompt("s2"), &feeds, true, yes), None);
        // One named already is told its name, as its start would have.
        record(&feeds, "s3", "Tamsin Camden", None, || panic!()).unwrap();
        let told = first_prompt(&prompt("s3"), &feeds, true, yes).unwrap();
        assert!(told.contains("Tamsin Camden"), "{told}");
        // Off at its start, so neither asked nor marked: on later, its next
        // prompt asks.
        let s4 = json!({"session_id": "s4", "hook_event_name": "SessionStart"});
        assert_eq!(session_start(&s4, &feeds, false, true), None);
        assert!(first_prompt(&prompt("s4"), &feeds, true, yes).is_some());
        let _ = std::fs::remove_dir_all(&feeds);
    }
}
