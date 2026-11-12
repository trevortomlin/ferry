// Application state and routing.
//
// One struct, mutated only from the UI thread, holding no locks and no I/O
// handles. The UI reads it to draw and calls methods on it to change it; the
// engine touches it only through `message::Event`. Keeping it plain data means
// the interesting logic here is directly testable.

use crate::model::browser::Browser;
use crate::model::connection::{Connection, ConnectionId, Protocol};
use crate::model::store;

/// What the central pane is showing. Modal editing is separate — see
/// `Bookmarks::editor` — because a dialog sits *over* a route rather than
/// replacing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// Nothing open yet.
    #[default]
    Welcome,
    /// Browsing the selected connection.
    Browser,
}

/// A connection being edited. Separate from the saved list so cancelling
/// genuinely discards, and so a half-filled form is never written to disk.
#[derive(Debug, Clone)]
pub struct Draft {
    pub connection: Connection,
    /// Typed password, key passphrase, or secret access key. Lives only here
    /// until milestone 3 hands it to the OS keychain on save; it is never
    /// serialised.
    pub secret: String,
    /// False when adding, true when editing an existing entry.
    pub existing: bool,
    /// Set once the keychain confirms it already holds a secret for this
    /// connection, so the editor can offer "leave blank to keep it" instead of
    /// making the user retype a password it already has.
    pub secret_stored: bool,
}

impl Draft {
    pub fn title(&self) -> &'static str {
        if self.existing {
            "Edit connection"
        } else {
            "New connection"
        }
    }
}

#[derive(Debug, Default)]
pub struct Bookmarks {
    connections: Vec<Connection>,
    pub selected: Option<ConnectionId>,
    pub editor: Option<Draft>,
    pub view: View,
    pub browser: Browser,
    pub filter: String,
    pub status: String,
    /// Off by default so tests never touch the real config file. `load` turns
    /// it on, which is the only way a running app constructs this.
    persistent: bool,
}

impl Bookmarks {
    pub fn load() -> Self {
        let connections = store::load();
        let status = match connections.len() {
            0 => "No saved connections".to_owned(),
            1 => "1 saved connection".to_owned(),
            n => format!("{n} saved connections"),
        };
        Self {
            connections,
            status,
            persistent: true,
            ..Default::default()
        }
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn get(&self, id: ConnectionId) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn selected_connection(&self) -> Option<&Connection> {
        self.selected.and_then(|id| self.get(id))
    }

    // ---- routing ----

    pub fn select(&mut self, id: ConnectionId) {
        if self.get(id).is_some() {
            self.selected = Some(id);
            self.view = View::Browser;
        }
    }

    pub fn go_welcome(&mut self) {
        self.selected = None;
        self.view = View::Welcome;
    }

    // ---- editing ----

    pub fn begin_new(&mut self, protocol: Protocol) {
        self.editor = Some(Draft {
            connection: Connection::new(self.fresh_id(), protocol),
            secret: String::new(),
            existing: false,
            secret_stored: false,
        });
    }

    pub fn begin_edit(&mut self, id: ConnectionId) {
        if let Some(connection) = self.get(id) {
            self.editor = Some(Draft {
                connection: connection.clone(),
                secret: String::new(),
                existing: true,
                secret_stored: false,
            });
        }
    }

    /// Copy a connection into the editor under a new id, so a server that
    /// differs by one field does not have to be typed out again. The copy has
    /// its own id, so it gets its own keychain entry rather than sharing one.
    pub fn duplicate(&mut self, id: ConnectionId) {
        let Some(original) = self.get(id) else {
            return;
        };
        let mut copy = original.clone();
        copy.id = self.fresh_id();
        copy.name = format!("{} copy", original.name);
        self.editor = Some(Draft {
            connection: copy,
            secret: String::new(),
            existing: false,
            secret_stored: false,
        });
    }

    pub fn cancel_edit(&mut self) {
        self.editor = None;
    }

    /// Saves the open draft if it validates. Returns false and keeps the editor
    /// open otherwise, so the user is never silently dropped back to a list
    /// missing the thing they just typed.
    pub fn commit_edit(&mut self) -> bool {
        let Some(draft) = &self.editor else {
            return false;
        };
        if !draft.connection.problems().is_empty() {
            return false;
        }

        let connection = draft.connection.clone();
        let id = connection.id;
        match self.connections.iter_mut().find(|c| c.id == id) {
            Some(existing) => *existing = connection,
            None => self.connections.push(connection),
        }
        self.sort_connections();
        self.editor = None;
        self.selected = Some(id);
        self.persist();
        true
    }

    pub fn remove(&mut self, id: ConnectionId) {
        self.connections.retain(|c| c.id != id);
        if self.selected == Some(id) {
            self.go_welcome();
        }
        self.persist();
    }

    // ---- internals ----

    fn sort_connections(&mut self) {
        self.connections.sort_by_key(|c| c.name.to_lowercase());
    }

    /// Ids must never be reused: they key the keychain entry, and handing a new
    /// connection an old id would hand it someone else's password. Clock-based
    /// with a linear probe on the astronomically unlikely collision.
    fn fresh_id(&self) -> ConnectionId {
        let mut candidate = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        while candidate == 0 || self.connections.iter().any(|c| c.id.0 == candidate) {
            candidate = candidate.wrapping_add(1);
        }
        ConnectionId(candidate)
    }

    fn persist(&mut self) {
        if !self.persistent {
            return;
        }
        if let Err(error) = store::save(&self.connections) {
            tracing::error!(%error, "could not save connections");
            self.status = format!("Could not save connections: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::connection::Auth;

    /// Bypasses `load`/`persist` so tests never touch the real config file.
    fn blank() -> Bookmarks {
        Bookmarks::default()
    }

    fn fill(draft: &mut Draft, name: &str) {
        draft.connection.name = name.to_owned();
        draft.connection.host = "example.com".to_owned();
        draft.connection.username = "ferry".to_owned();
    }

    #[test]
    fn a_valid_draft_is_added_and_selected() {
        let mut state = blank();
        state.begin_new(Protocol::Sftp);
        fill(state.editor.as_mut().unwrap(), "Box");

        assert!(state.commit_edit());
        assert_eq!(state.connections().len(), 1);
        assert_eq!(state.connections()[0].name, "Box");
        assert_eq!(state.selected, Some(state.connections()[0].id));
        assert!(state.editor.is_none());
    }

    #[test]
    fn an_invalid_draft_is_rejected_and_the_editor_stays_open() {
        let mut state = blank();
        state.begin_new(Protocol::Sftp);

        assert!(!state.commit_edit());
        assert!(state.connections().is_empty());
        assert!(state.editor.is_some(), "the typed values must not be lost");
    }

    #[test]
    fn editing_replaces_rather_than_duplicates() {
        let mut state = blank();
        state.begin_new(Protocol::Sftp);
        fill(state.editor.as_mut().unwrap(), "Box");
        state.commit_edit();
        let id = state.connections()[0].id;

        state.begin_edit(id);
        state.editor.as_mut().unwrap().connection.port = 2222;
        assert!(state.commit_edit());

        assert_eq!(state.connections().len(), 1);
        assert_eq!(state.connections()[0].port, 2222);
        assert_eq!(state.connections()[0].id, id);
    }

    #[test]
    fn cancelling_discards_everything_typed() {
        let mut state = blank();
        state.begin_new(Protocol::Sftp);
        fill(state.editor.as_mut().unwrap(), "Box");
        state.cancel_edit();

        assert!(state.connections().is_empty());
        assert!(state.editor.is_none());
    }

    #[test]
    fn drafts_never_carry_a_secret_into_the_saved_connection() {
        let mut state = blank();
        state.begin_new(Protocol::Sftp);
        let draft = state.editor.as_mut().unwrap();
        fill(draft, "Box");
        draft.connection.auth = Auth::Password;
        draft.secret = "hunter2".to_owned();
        state.commit_edit();

        let saved = &state.connections()[0];
        let text = toml::to_string(saved).unwrap();
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[test]
    fn removing_the_selected_connection_returns_to_welcome() {
        let mut state = blank();
        state.begin_new(Protocol::Sftp);
        fill(state.editor.as_mut().unwrap(), "Box");
        state.commit_edit();
        let id = state.connections()[0].id;

        state.select(id);
        assert_eq!(state.view, View::Browser);

        state.remove(id);
        assert!(state.connections().is_empty());
        assert_eq!(state.view, View::Welcome);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn selecting_an_unknown_id_does_nothing() {
        let mut state = blank();
        state.select(ConnectionId(999));
        assert_eq!(state.selected, None);
        assert_eq!(state.view, View::Welcome);
    }

    #[test]
    fn connections_are_kept_in_name_order() {
        let mut state = blank();
        for name in ["zulu", "Alpha", "mike"] {
            state.begin_new(Protocol::Sftp);
            fill(state.editor.as_mut().unwrap(), name);
            state.commit_edit();
        }
        let names: Vec<_> = state
            .connections()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["Alpha", "mike", "zulu"]);
    }

    #[test]
    fn fresh_ids_do_not_collide_with_existing_ones() {
        let mut state = blank();
        for name in ["a", "b", "c", "d"] {
            state.begin_new(Protocol::Sftp);
            fill(state.editor.as_mut().unwrap(), name);
            state.commit_edit();
        }
        let mut ids: Vec<_> = state.connections().iter().map(|c| c.id).collect();
        ids.sort();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique);
    }
}
