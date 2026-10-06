//! Smart playlist rules, and how they become SQL.
//!
//! The schema has carried `playlists.kind` and `playlists.rules` since the
//! first migration, so this needs none of its own: a smart playlist is a
//! playlist row whose `kind` is `smart` and whose `rules` hold the JSON below.
//!
//! **Nothing the user typed is ever put into a statement.** `Field` and `Op`
//! are closed enums that map to constant SQL fragments; the value from each
//! rule leaves here as a bound parameter and nothing else. That is the same
//! discipline the rest of `db::queries` follows, and it is the reason this
//! module compiles to `(String, Vec<Value>)` rather than to a finished query.

use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

/// What a rule looks at. Closed on purpose — a field that is not in this list
/// cannot be asked for, so there is no path from user input to a column name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Field {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Genre,
    Composer,
    Comment,
    Year,
    Bpm,
    PlayCount,
    Rating,
    Duration,
    Favorite,

    // -- listening habits --
    //
    // These are what the automatic playlists are built from, and the reason
    // they read "days ago" rather than a date: a rule set is stored once and
    // answered every time the playlist is opened, so anything anchored to a
    // fixed date would go stale the day after it was written.
    /// Whether this track has ever been started at all, as 1 or 0.
    ///
    /// Not the same question as `PlayCount`, and the difference is the one
    /// that matters here: a track you put on and skipped has a play count of
    /// zero, because a play means hearing half of it. "Never played" has to
    /// mean never put on, which is what this reads.
    EverPlayed,
    /// Days since this track was last started. Never started is NULL, which no
    /// comparison matches — pair it with `EverPlayed` instead.
    DaysSincePlayed,
    /// Days since it was added to the library.
    DaysSinceAdded,
    /// Real plays in the last month — skips excluded, so this agrees with the
    /// play count rather than with the raw history.
    PlaysThisMonth,
    /// Years between the track's year and this one. Relative, like the other
    /// habits, so "ten years or more" does not go stale. No year is NULL.
    YearsAgo,
}

impl Field {
    pub const ALL: [Self; 18] = [
        Self::Title,
        Self::Artist,
        Self::AlbumArtist,
        Self::Album,
        Self::Genre,
        Self::Composer,
        Self::Comment,
        Self::Year,
        Self::Bpm,
        Self::PlayCount,
        Self::Rating,
        Self::Duration,
        Self::Favorite,
        Self::EverPlayed,
        Self::DaysSincePlayed,
        Self::DaysSinceAdded,
        Self::PlaysThisMonth,
        Self::YearsAgo,
    ];

    /// The SQL this field reads. A constant, always — that is what makes the
    /// compiler injection-proof rather than injection-careful.
    ///
    /// The aliases match `TRACK_SELECT`, so a compiled clause drops into any
    /// query that joins `albums al` and `genres g` the same way.
    fn column(self) -> &'static str {
        match self {
            Self::Title => "t.title",
            Self::Artist => "t.artist",
            Self::AlbumArtist => "COALESCE(NULLIF(TRIM(t.album_artist), ''), t.artist)",
            Self::Album => "COALESCE(al.name, '')",
            Self::Genre => "COALESCE(g.name, '')",
            Self::Composer => "COALESCE(t.composer, '')",
            Self::Comment => "COALESCE(t.comment, '')",
            Self::Year => "t.year",
            Self::Bpm => "t.bpm",
            Self::PlayCount => "t.play_count",
            Self::Rating => "t.rating",
            Self::Duration => "t.duration",
            Self::Favorite => "t.favorite",

            // SQLite's own clock rather than a bound "now", so these stay the
            // constant fragments the rest of this module depends on — and so a
            // stored rule means the same thing whenever it is asked.
            // `last_played` is stamped when a track starts, so this is "was it
            // ever put on", not "was it ever finished".
            Self::EverPlayed => "(t.last_played IS NOT NULL)",
            Self::DaysSincePlayed => {
                "((strftime('%s','now') * 1000.0 - t.last_played) / 86400000.0)"
            }
            Self::DaysSinceAdded => "((strftime('%s','now') * 1000.0 - t.date_added) / 86400000.0)",
            // 2_592_000_000 is thirty days in milliseconds, spelled out rather
            // than named because this has to be one `&'static str`. The 0.5 is
            // `models::PLAY_THRESHOLD`, spelled out for the same reason and
            // pinned to it by a test — without it this counts skips, and a
            // track skipped three times reads as one you have on repeat.
            Self::PlaysThisMonth => {
                "(SELECT COUNT(*) FROM play_history ph
                   WHERE ph.track_id = t.id
                     AND ph.completion >= 0.5
                     AND ph.played_at >= strftime('%s','now') * 1000.0 - 2592000000.0)"
            }
            Self::YearsAgo => "(CAST(strftime('%Y','now') AS INTEGER) - t.year)",
        }
    }

    /// Numbers compare; text matches. Which one this is decides both the
    /// operators the editor offers and how the value is bound.
    pub fn is_number(self) -> bool {
        matches!(
            self,
            Self::Year
                | Self::Bpm
                | Self::PlayCount
                | Self::Rating
                | Self::Duration
                | Self::Favorite
                | Self::EverPlayed
                | Self::DaysSincePlayed
                | Self::DaysSinceAdded
                | Self::PlaysThisMonth
                | Self::YearsAgo
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::AlbumArtist => "Album artist",
            Self::Album => "Album",
            Self::Genre => "Genre",
            Self::Composer => "Composer",
            Self::Comment => "Comment",
            Self::Year => "Year",
            Self::Bpm => "BPM",
            Self::PlayCount => "Play count",
            Self::Rating => "Rating",
            Self::Duration => "Length (seconds)",
            Self::Favorite => "Favorite (1 or 0)",
            Self::EverPlayed => "Ever played (1 or 0)",
            Self::DaysSincePlayed => "Days since last played",
            Self::DaysSinceAdded => "Days since added",
            Self::PlaysThisMonth => "Plays in the last 30 days",
            Self::YearsAgo => "Years since release",
        }
    }
}

/// How a rule compares. Also closed, for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Is,
    IsNot,
    Contains,
    DoesNotContain,
    StartsWith,
    EndsWith,
    MoreThan,
    LessThan,
}

impl Op {
    /// Which operators make sense for a field. "Contains" on a play count is
    /// not a question anyone means to ask.
    pub fn for_field(field: Field) -> &'static [Self] {
        match field.is_number() {
            true => &[Self::Is, Self::IsNot, Self::MoreThan, Self::LessThan],
            false => &[
                Self::Is,
                Self::IsNot,
                Self::Contains,
                Self::DoesNotContain,
                Self::StartsWith,
                Self::EndsWith,
            ],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Is => "is",
            Self::IsNot => "is not",
            Self::Contains => "contains",
            Self::DoesNotContain => "does not contain",
            Self::StartsWith => "starts with",
            Self::EndsWith => "ends with",
            Self::MoreThan => "more than",
            Self::LessThan => "less than",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub field: Field,
    pub op: Op,
    /// Whatever the user typed. It is never anything but a bound parameter.
    pub value: String,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            field: Field::Artist,
            op: Op::Contains,
            value: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartRules {
    /// All the rules, or any of them.
    pub match_all: bool,
    pub rules: Vec<Rule>,
    /// At most this many tracks, most recently added first. `None` is
    /// everything that matches.
    pub limit: Option<u32>,
}

impl Default for SmartRules {
    fn default() -> Self {
        Self {
            match_all: true,
            rules: vec![],
            limit: None,
        }
    }
}

impl SmartRules {
    /// Reads the JSON out of `playlists.rules`. An unreadable rule set matches
    /// everything rather than erroring: a smart playlist showing the whole
    /// library is obviously wrong and can be fixed, where one that refuses to
    /// open cannot.
    pub fn parse(json: Option<&str>) -> Self {
        let Some(json) = json else {
            return Self::default();
        };
        serde_json::from_str(json).unwrap_or_else(|error| {
            log::warn!("library: a smart playlist's rules are unreadable: {error:#}");
            Self::default()
        })
    }

    pub fn to_json(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }
}

/// The smart playlists the app makes for itself, in the order they appear.
///
/// All are about listening or length rather than about tags — what is on
/// repeat, what has been dropped, what was never given a chance. Deliberately none of
/// them repeat Home, which already carries Recently Added, Recently Played and
/// Most Played, nor the Favorites entry in the sidebar.
///
/// They are ordinary smart playlists once created: editable, renameable,
/// deletable. Nothing puts them back afterwards.
pub fn automatic() -> Vec<(&'static str, SmartRules)> {
    let rule = |field: Field, op: Op, value: &str| Rule {
        field,
        op,
        value: value.to_owned(),
    };
    let all = |rules: Vec<Rule>, limit: Option<u32>| SmartRules {
        match_all: true,
        rules,
        limit,
    };

    vec![
        // What you are actually listening to at the moment.
        (
            "On Repeat",
            all(
                vec![rule(Field::PlaysThisMonth, Op::MoreThan, "2")],
                Some(50),
            ),
        ),
        // Played plenty, once, and then not for a season.
        (
            "Rediscover",
            all(
                vec![
                    rule(Field::PlayCount, Op::MoreThan, "4"),
                    rule(Field::DaysSincePlayed, Op::MoreThan, "90"),
                ],
                Some(50),
            ),
        ),
        // Songs you told us you loved and have not played in two months.
        (
            "Forgotten Favourites",
            all(
                vec![
                    rule(Field::Favorite, Op::Is, "1"),
                    rule(Field::DaysSincePlayed, Op::MoreThan, "60"),
                ],
                None,
            ),
        ),
        // Never put on at all. Deliberately not `PlayCount is 0`, which is a
        // different question: a track started and skipped never reaches half,
        // so its play count stays zero and it would sit in here for ever
        // despite having been played.
        (
            "Never Played",
            all(vec![rule(Field::EverPlayed, Op::Is, "0")], None),
        ),
        // New in the library and not yet put on.
        (
            "Fresh Finds",
            all(
                vec![
                    rule(Field::DaysSinceAdded, Op::LessThan, "14"),
                    rule(Field::EverPlayed, Op::Is, "0"),
                ],
                None,
            ),
        ),
        (
            "Heavy Rotation",
            all(vec![rule(Field::PlayCount, Op::MoreThan, "9")], None),
        ),
        // Seconds, per the Duration field.
        (
            "Quick Hits",
            all(vec![rule(Field::Duration, Op::LessThan, "180")], None),
        ),
        (
            "Long Listens",
            all(vec![rule(Field::Duration, Op::MoreThan, "480")], None),
        ),
        // Old songs you have been playing lately. More than 9 is ten or more.
        (
            "Throwbacks",
            all(
                vec![
                    rule(Field::YearsAgo, Op::MoreThan, "9"),
                    rule(Field::PlaysThisMonth, Op::MoreThan, "0"),
                ],
                None,
            ),
        ),
    ]
}

/// The `WHERE` fragment for a rule set, and the values to bind to it.
///
/// The fragment is built from constants; every value is a `?`. A rule set with
/// nothing usable in it compiles to `1`, which matches everything — the same
/// answer an empty filter gives anywhere else.
pub fn compile(rules: &SmartRules) -> (String, Vec<Value>) {
    let mut fragments: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();

    for rule in &rules.rules {
        // A half-typed rule is not a rule yet. Skipping it means the editor can
        // show an empty row without the list emptying underneath it.
        if rule.value.trim().is_empty() {
            continue;
        }
        let Some((fragment, value)) = one(rule) else {
            continue;
        };
        fragments.push(fragment);
        values.push(value);
    }

    if fragments.is_empty() {
        return ("1".to_owned(), values);
    }
    let joiner = match rules.match_all {
        true => " AND ",
        false => " OR ",
    };
    (format!("({})", fragments.join(joiner)), values)
}

/// One rule as a fragment and its bound value, or `None` when the value does
/// not fit the field — "more than banana" is not a question about a year.
fn one(rule: &Rule) -> Option<(String, Value)> {
    let column = rule.field.column();
    let text = rule.value.trim();

    if rule.field.is_number() {
        let number: f64 = text.parse().ok()?;
        let comparison = match rule.op {
            Op::Is => "=",
            Op::IsNot => "!=",
            Op::MoreThan => ">",
            Op::LessThan => "<",
            // The editor does not offer these for a number; a hand-edited rule
            // file could still name one, and equality is the closest honest
            // reading of it.
            _ => "=",
        };
        return Some((format!("{column} {comparison} ?"), Value::Real(number)));
    }

    let (fragment, pattern) = match rule.op {
        Op::Is => (format!("{column} = ? COLLATE NOCASE"), text.to_owned()),
        Op::IsNot => (format!("{column} != ? COLLATE NOCASE"), text.to_owned()),
        Op::Contains => (
            format!("{column} LIKE ? ESCAPE '\\' COLLATE NOCASE"),
            format!("%{}%", escape_like(text)),
        ),
        Op::DoesNotContain => (
            format!("{column} NOT LIKE ? ESCAPE '\\' COLLATE NOCASE"),
            format!("%{}%", escape_like(text)),
        ),
        Op::StartsWith => (
            format!("{column} LIKE ? ESCAPE '\\' COLLATE NOCASE"),
            format!("{}%", escape_like(text)),
        ),
        Op::EndsWith => (
            format!("{column} LIKE ? ESCAPE '\\' COLLATE NOCASE"),
            format!("%{}", escape_like(text)),
        ),
        // Numeric operators on text: compared as text, which is what SQLite
        // would do anyway. Not offered by the editor.
        Op::MoreThan => (format!("{column} > ?"), text.to_owned()),
        Op::LessThan => (format!("{column} < ?"), text.to_owned()),
    };
    Some((fragment, Value::Text(pattern)))
}

/// `%`, `_` and `\` are wildcards to `LIKE`; a user asking for them means the
/// characters themselves.
fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(field: Field, op: Op, value: &str) -> Rule {
        Rule {
            field,
            op,
            value: value.to_owned(),
        }
    }

    #[test]
    fn a_hostile_value_stays_bound_and_cannot_reach_the_statement() {
        let rules = SmartRules {
            match_all: true,
            rules: vec![rule(Field::Artist, Op::Is, "'; DROP TABLE tracks; --")],
            limit: None,
        };
        let (clause, values) = compile(&rules);

        // The clause is built from constants only: the value is a `?`.
        assert_eq!(clause, "(t.artist = ? COLLATE NOCASE)");
        assert!(!clause.contains("DROP"));
        assert!(!clause.contains('\''));
        assert_eq!(
            values,
            vec![Value::Text("'; DROP TABLE tracks; --".to_owned())]
        );
    }

    #[test]
    fn wildcards_in_a_value_are_characters_rather_than_patterns() {
        let (_, values) = compile(&SmartRules {
            match_all: true,
            rules: vec![rule(Field::Title, Op::Contains, "100%_real")],
            limit: None,
        });
        assert_eq!(values, vec![Value::Text(r"%100\%\_real%".to_owned())]);
    }

    #[test]
    fn rules_join_on_all_or_any() {
        let both = vec![
            rule(Field::Artist, Op::Is, "Yes"),
            rule(Field::Year, Op::MoreThan, "1970"),
        ];
        let (all, _) = compile(&SmartRules {
            match_all: true,
            rules: both.clone(),
            limit: None,
        });
        assert_eq!(all, "(t.artist = ? COLLATE NOCASE AND t.year > ?)");

        let (any, _) = compile(&SmartRules {
            match_all: false,
            rules: both,
            limit: None,
        });
        assert_eq!(any, "(t.artist = ? COLLATE NOCASE OR t.year > ?)");
    }

    #[test]
    fn unusable_rules_are_skipped_rather_than_breaking_the_query() {
        // An empty value is a rule the user has not finished typing, and a
        // number field given words is one they never will.
        let (clause, values) = compile(&SmartRules {
            match_all: true,
            rules: vec![
                rule(Field::Artist, Op::Contains, "   "),
                rule(Field::Year, Op::MoreThan, "banana"),
            ],
            limit: None,
        });
        assert_eq!(clause, "1");
        assert!(values.is_empty());
    }

    #[test]
    fn rules_survive_a_round_trip_through_the_rules_column() {
        let rules = SmartRules {
            match_all: false,
            rules: vec![rule(Field::Genre, Op::StartsWith, "Prog")],
            limit: Some(25),
        };
        let json = rules.to_json().unwrap();
        assert_eq!(SmartRules::parse(Some(&json)), rules);

        // Nothing stored, or something unreadable, matches everything rather
        // than refusing to open.
        assert_eq!(SmartRules::parse(None), SmartRules::default());
        assert_eq!(SmartRules::parse(Some("{{{")), SmartRules::default());
    }

    #[test]
    fn a_number_field_only_offers_comparisons() {
        assert!(!Op::for_field(Field::Year).contains(&Op::Contains));
        assert!(Op::for_field(Field::Artist).contains(&Op::Contains));
    }
}
