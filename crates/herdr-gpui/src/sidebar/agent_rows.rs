//! The daemon's `[ui.sidebar.agents]` rules, as the terminal client reads
//! them: which tokens each agent row shows, in what order, with which inline
//! styling. One table drives both clients, so their agent panels agree.

use herdr_client::protocol::{AgentStatus, ClientShellAgent};
use std::collections::BTreeMap;

/// Upstream's default rows: status, host, workspace, tab, then the agent.
const DEFAULT_ROWS: &[&[&str]] = &[&["state_icon", "machine", "workspace", "tab"], &["agent"]];
/// Upstream's own caps, so a table it accepts this client also accepts.
const MAX_ROWS: usize = 8;
const MAX_RULES_PER_ROW: usize = 16;
const MAX_ROW_GAP: u8 = 4;

/// A token an agent row may show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    StateIcon,
    StateText,
    Machine,
    Workspace,
    Tab,
    Pane,
    Agent,
    TerminalTitle,
    TerminalTitleStripped,
    /// Pane metadata reported as `$name`.
    Custom(String),
}

impl Token {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "state_icon" => Self::StateIcon,
            "state_text" => Self::StateText,
            "machine" => Self::Machine,
            "workspace" => Self::Workspace,
            "tab" => Self::Tab,
            "pane" => Self::Pane,
            "agent" => Self::Agent,
            "terminal_title" => Self::TerminalTitle,
            "terminal_title_stripped" => Self::TerminalTitleStripped,
            name => Self::Custom(
                name.strip_prefix('$')
                    .filter(|name| !name.is_empty())?
                    .into(),
            ),
        })
    }
}

/// A named color the theme resolves; hex and `rgb()` arrive as `Hex`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Color {
    Accent,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    Hex(u32),
}

impl Color {
    fn parse(text: &str) -> Option<Self> {
        if let Some(hex) = text.strip_prefix('#') {
            let value = u32::from_str_radix(hex, 16).ok()?;
            return match hex.len() {
                3 => Some(Self::Hex(
                    (((value >> 8) & 0xf) * 0x11) << 16
                        | (((value >> 4) & 0xf) * 0x11) << 8
                        | ((value & 0xf) * 0x11),
                )),
                6 => Some(Self::Hex(value)),
                _ => None,
            };
        }
        if let Some(rgb) = text
            .strip_prefix("rgb(")
            .and_then(|text| text.strip_suffix(')'))
        {
            let mut parts = rgb
                .split(',')
                .map(|part| part.trim().parse::<u32>().ok().filter(|part| *part <= 255));
            let (red, green, blue) = (parts.next()??, parts.next()??, parts.next()??);
            if parts.next().is_some() {
                return None;
            }
            return Some(Self::Hex((red << 16) | (green << 8) | blue));
        }
        Some(match text {
            "accent" => Self::Accent,
            "black" => Self::Black,
            "red" => Self::Red,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "blue" => Self::Blue,
            "magenta" => Self::Magenta,
            "cyan" => Self::Cyan,
            "white" => Self::White,
            _ => return None,
        })
    }
}

/// One token occurrence: what to show, and styles that override the
/// contextual defaults an omitted field inherits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    pub(crate) token: Token,
    pub(crate) fg: Option<Color>,
    pub(crate) bold: Option<bool>,
    pub(crate) dim: Option<bool>,
}

/// The rows every agent paints, plus per-agent overrides and the blank-line
/// gap between entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentRows {
    rows: Vec<Vec<Rule>>,
    by_agent: BTreeMap<String, Vec<Vec<Rule>>>,
    pub(crate) gap: u8,
}

impl Default for AgentRows {
    fn default() -> Self {
        Self {
            rows: default_rows(),
            by_agent: BTreeMap::new(),
            gap: 0,
        }
    }
}

impl AgentRows {
    /// Reads `[ui.sidebar.agents]` from the daemon's parsed config. Anything
    /// unreadable leaves the defaults alone: the file may come from a newer
    /// Herdr, exactly as it may for `[ui.toast.clipboard]`.
    pub(crate) fn from_daemon(root: &toml::Table) -> Self {
        let agents = root
            .get("ui")
            .and_then(|ui| ui.get("sidebar"))
            .and_then(|sidebar| sidebar.get("agents"))
            .and_then(toml::Value::as_table);
        let Some(agents) = agents else {
            return Self::default();
        };
        let rows = agents
            .get("rows")
            .and_then(parse_rows)
            .unwrap_or_else(default_rows);
        let mut by_agent = BTreeMap::new();
        if let Some(map) = agents.get("rows_by_agent").and_then(toml::Value::as_table) {
            for (name, value) in map {
                if let Some(rows) = parse_rows(value) {
                    by_agent.insert(name.clone(), rows);
                }
            }
        }
        let gap = agents
            .get("row_gap")
            .and_then(toml::Value::as_integer)
            .and_then(|gap| u8::try_from(gap).ok())
            .filter(|gap| *gap <= MAX_ROW_GAP)
            .unwrap_or(0);
        Self {
            rows,
            by_agent,
            gap,
        }
    }

    /// The rows an agent paints: its canonical id's override, or the shared
    /// set. A canonical id is what `agent` carries, as upstream matches it.
    pub(crate) fn for_agent(&self, agent: Option<&str>) -> &[Vec<Rule>] {
        agent
            .and_then(|agent| self.by_agent.get(agent))
            .map_or(self.rows.as_slice(), Vec::as_slice)
    }
}

/// One painted piece of a rule. The icon keeps GPUI's drawn status dot, which
/// is its `state_icon`; everything else is a text span.
pub(crate) enum Piece<'a> {
    Icon,
    Text { text: &'a str },
}

impl Rule {
    /// What this rule shows for one agent, or `None` when the token has
    /// nothing to say here: a missing host, tab, title, or metadata key.
    pub(crate) fn resolve<'a>(
        &self,
        agent: &'a ClientShellAgent,
        name: &'a str,
        place: Option<(&'a str, Option<&'a str>)>,
        host: Option<&'a str>,
        pane_label: Option<&'a str>,
    ) -> Option<Piece<'a>> {
        let text = |text: &'a str| (!text.is_empty()).then_some(Piece::Text { text });
        match &self.token {
            Token::StateIcon => Some(Piece::Icon),
            Token::StateText => text(state_text(agent)),
            Token::Machine => text(host?),
            Token::Workspace => text(place?.0),
            Token::Tab => text(place?.1?),
            Token::Pane => text(pane_label?),
            Token::Agent => text(name),
            Token::TerminalTitle => text(agent.terminal_title.as_deref()?),
            Token::TerminalTitleStripped => text(agent.terminal_title_stripped.as_deref()?),
            Token::Custom(token) => text(
                agent
                    .tokens
                    .iter()
                    .find(|(name, _)| name == token)
                    .map(|(_, value)| value.as_str())?,
            ),
        }
    }
}

/// The status word `state_labels` keys on, which upstream prefers over its
/// own default when the pane reported one.
fn state_text(agent: &ClientShellAgent) -> &str {
    let word = match agent.agent_status {
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Working => "working",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    };
    agent
        .state_labels
        .iter()
        .find(|(state, _)| state == word)
        .map_or(word, |(_, label)| label.as_str())
}

fn default_rows() -> Vec<Vec<Rule>> {
    DEFAULT_ROWS
        .iter()
        .map(|row| {
            row.iter()
                .filter_map(|name| Token::parse(name))
                .map(|token| Rule {
                    token,
                    fg: None,
                    bold: None,
                    dim: None,
                })
                .collect()
        })
        .collect()
}

fn parse_rows(value: &toml::Value) -> Option<Vec<Vec<Rule>>> {
    let rows = value.as_array()?;
    if rows.is_empty() || rows.len() > MAX_ROWS {
        return None;
    }
    rows.iter().map(parse_row).collect()
}

fn parse_row(value: &toml::Value) -> Option<Vec<Rule>> {
    let cells = value.as_array()?;
    if cells.is_empty() || cells.len() > MAX_RULES_PER_ROW {
        return None;
    }
    cells.iter().map(parse_rule).collect()
}

fn parse_rule(value: &toml::Value) -> Option<Rule> {
    match value {
        toml::Value::String(name) => Some(Rule {
            token: Token::parse(name)?,
            fg: None,
            bold: None,
            dim: None,
        }),
        toml::Value::Table(table) => {
            let fg = match table.get("fg") {
                Some(fg) => Some(Color::parse(fg.as_str()?)?),
                None => None,
            };
            let bold = match table.get("bold") {
                Some(bold) => Some(bold.as_bool()?),
                None => None,
            };
            let dim = match table.get("dim") {
                Some(dim) => Some(dim.as_bool()?),
                None => None,
            };
            Some(Rule {
                token: Token::parse(table.get("token")?.as_str()?)?,
                fg,
                bold,
                dim,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn parse(text: &str) -> AgentRows {
        AgentRows::from_daemon(&text.parse::<toml::Table>().unwrap())
    }

    fn names(rows: &[Vec<Rule>]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|rule| match &rule.token {
                        Token::Custom(name) => format!("${name}"),
                        Token::StateIcon => "state_icon".into(),
                        Token::StateText => "state_text".into(),
                        Token::Machine => "machine".into(),
                        Token::Workspace => "workspace".into(),
                        Token::Tab => "tab".into(),
                        Token::Pane => "pane".into(),
                        Token::Agent => "agent".into(),
                        Token::TerminalTitle => "terminal_title".into(),
                        Token::TerminalTitleStripped => "terminal_title_stripped".into(),
                    })
                    .collect()
            })
            .collect()
    }

    fn rows_of(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|name| (*name).to_owned()).collect())
            .collect()
    }

    #[test]
    fn default_rows_match_upstream_and_parameterize_by_agent() {
        let rows = AgentRows::default();
        assert_eq!(
            names(&rows.rows.clone()),
            rows_of(&[&["state_icon", "machine", "workspace", "tab"], &["agent"]])
        );
        assert_eq!(rows.gap, 0);
        assert_eq!(rows.for_agent(Some("claude")), rows.rows);
    }

    #[test]
    fn daemon_rows_styles_overrides_and_gap_are_read() {
        let rows = parse(
            r##"
            [ui.sidebar.agents]
            row_gap = 1
            rows = [
              ["state_icon", { token = "tab", fg = "#bac2de", dim = false }],
              [{ token = "agent", bold = false }],
            ]
            [ui.sidebar.agents.rows_by_agent]
            claude = [["state_icon", "workspace"], ["agent"]]
            "##,
        );
        assert_eq!(rows.gap, 1);
        assert_eq!(
            names(&rows.rows),
            rows_of(&[&["state_icon", "tab"], &["agent"]])
        );
        assert_eq!(rows.rows[0][1].fg, Some(Color::Hex(0xbac2de)));
        assert_eq!(rows.rows[0][1].dim, Some(false));
        assert_eq!(rows.rows[1][0].bold, Some(false));
        assert_eq!(
            names(rows.for_agent(Some("claude"))),
            rows_of(&[&["state_icon", "workspace"], &["agent"]])
        );
        assert_eq!(names(rows.for_agent(Some("codex"))), names(&rows.rows));
    }

    #[test]
    fn colors_accept_names_hex_and_rgb() {
        assert_eq!(Color::parse("cyan"), Some(Color::Cyan));
        assert_eq!(Color::parse("#89b4fa"), Some(Color::Hex(0x89b4fa)));
        assert_eq!(Color::parse("#fff"), Some(Color::Hex(0xffffff)));
        assert_eq!(Color::parse("rgb(1, 2, 3)"), Some(Color::Hex(0x010203)));
        assert_eq!(Color::parse("rgb(1,2,3,4)"), None);
        assert_eq!(Color::parse("rgb(999,2,3)"), None);
        assert_eq!(Color::parse("chartreuse"), None);
    }

    #[test]
    fn malformed_rules_leave_defaults_or_drop_the_override() {
        for bad in [
            "[ui.sidebar.agents]\nrows = []",
            "[ui.sidebar.agents]\nrows = [\"not-a-row\"]",
            "[ui.sidebar.agents]\nrows = [[\"unknown_token\"]]",
            "[ui.sidebar.agents]\nrows = [[{ fg = \"#fff\" }]]",
            "[ui.sidebar.agents]\nrows = [[{ token = \"agent\", bold = \"yes\" }]]",
            "[ui.sidebar.agents]\nrows = [[{ token = \"agent\", fg = \"nope\" }]]",
        ] {
            assert_eq!(parse(bad), AgentRows::default(), "{bad}");
        }
        let rows = parse(
            "[ui.sidebar.agents]\nrow_gap = 99\n[ui.sidebar.agents.rows_by_agent]\nclaude = [[\"unknown\"]]",
        );
        assert_eq!(rows.gap, 0);
        assert!(rows.by_agent.is_empty());
    }

    #[test]
    fn rule_and_row_caps_hold() {
        let long_row = (0..=MAX_RULES_PER_ROW)
            .map(|_| "\"agent\"")
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            parse(&format!("[ui.sidebar.agents]\nrows = [[{long_row}]]")),
            AgentRows::default()
        );
        let many_rows = (0..=MAX_ROWS)
            .map(|_| "[\"agent\"]")
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            parse(&format!("[ui.sidebar.agents]\nrows = [{many_rows}]")),
            AgentRows::default()
        );
    }
}
