use bone_app::{RequestId, SessionId};

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Status {
    text: String,
    owner: Owner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Owner {
    General,
    TitleEdit(SessionId),
    TitleRename(SessionId, u64),
    AutoTitle(SessionId, u64),
    Submission(SessionId, RequestId),
    Session(SessionId, u64),
    PanelRequest(Option<SessionId>, u64),
    Selection(Option<SessionId>),
    Create(RequestId),
    Open(SessionId, u64),
}

impl Status {
    fn owned(owner: Owner, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            owner,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn title_edit(session: SessionId, text: impl Into<String>) -> Self {
        Self::owned(Owner::TitleEdit(session), text)
    }

    pub(crate) fn title_rename(session: SessionId, request: u64, text: impl Into<String>) -> Self {
        Self::owned(Owner::TitleRename(session, request), text)
    }

    pub(crate) fn auto_title(session: SessionId, request: u64, text: impl Into<String>) -> Self {
        Self::owned(Owner::AutoTitle(session, request), text)
    }

    pub(crate) fn submission(
        session: SessionId,
        request: RequestId,
        text: impl Into<String>,
    ) -> Self {
        Self::owned(Owner::Submission(session, request), text)
    }

    pub(crate) fn session(session: SessionId, generation: u64, text: impl Into<String>) -> Self {
        Self::owned(Owner::Session(session, generation), text)
    }

    pub(crate) fn panel_request(
        session: Option<SessionId>,
        request: u64,
        text: impl Into<String>,
    ) -> Self {
        Self::owned(Owner::PanelRequest(session, request), text)
    }

    pub(crate) fn selection(session: Option<SessionId>, text: impl Into<String>) -> Self {
        Self::owned(Owner::Selection(session), text)
    }

    pub(crate) fn create(request: RequestId, text: impl Into<String>) -> Self {
        Self::owned(Owner::Create(request), text)
    }

    pub(crate) fn open(session: SessionId, generation: u64, text: impl Into<String>) -> Self {
        Self::owned(Owner::Open(session, generation), text)
    }

    pub(crate) fn belongs_to_title(&self, session: SessionId) -> bool {
        matches!(
            self.owner,
            Owner::TitleEdit(target)
                | Owner::TitleRename(target, _)
                | Owner::AutoTitle(target, _)
                if target == session
        )
    }

    pub(crate) fn belongs_to_title_rename(&self, session: SessionId, request: u64) -> bool {
        self.owner == Owner::TitleRename(session, request)
    }

    pub(crate) fn belongs_to_auto_title_at_or_before(
        &self,
        session: SessionId,
        request: u64,
    ) -> bool {
        matches!(self.owner, Owner::AutoTitle(target, status_request) if target == session && status_request <= request)
    }

    pub(crate) fn belongs_to_submission(&self, session: SessionId, request: RequestId) -> bool {
        self.owner == Owner::Submission(session, request)
    }

    pub(crate) fn belongs_to_panel_request(
        &self,
        session: Option<SessionId>,
        request: u64,
    ) -> bool {
        self.owner == Owner::PanelRequest(session, request)
    }

    pub(crate) fn belongs_to_create(&self, request: RequestId) -> bool {
        self.owner == Owner::Create(request)
    }

    pub(crate) fn belongs_to_open(&self, session: SessionId, generation: u64) -> bool {
        self.owner == Owner::Open(session, generation)
    }

    pub(super) fn is_current(
        &self,
        selected: Option<SessionId>,
        selected_generation: Option<u64>,
    ) -> bool {
        match self.owner {
            Owner::TitleEdit(session)
            | Owner::TitleRename(session, _)
            | Owner::AutoTitle(session, _)
            | Owner::Submission(session, _) => selected == Some(session),
            Owner::Session(session, generation) | Owner::Open(session, generation) => {
                selected == Some(session) && selected_generation == Some(generation)
            }
            Owner::PanelRequest(session, _) | Owner::Selection(session) => selected == session,
            Owner::General | Owner::Create(_) => true,
        }
    }
}

impl From<String> for Status {
    fn from(text: String) -> Self {
        Self::owned(Owner::General, text)
    }
}

impl From<&str> for Status {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}
