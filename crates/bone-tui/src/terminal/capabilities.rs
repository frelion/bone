/// The keyboard capability negotiated for this terminal session.
///
/// This value describes runtime I/O only. It never changes terminal, shell, or
/// multiplexer configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalCapabilities {
    #[cfg(windows)]
    /// Native Windows console events carry modifiers without a VT extension.
    NativeWindows,
    /// Kitty's progressive keyboard protocol, transported as CSI-u sequences.
    Kitty,
    /// The terminal cannot prove that Enter and Shift+Enter are distinct.
    Compatibility { reason: String },
}

impl TerminalCapabilities {
    #[cfg(windows)]
    pub(crate) fn native_windows() -> Self {
        Self::NativeWindows
    }

    pub(crate) fn kitty() -> Self {
        Self::Kitty
    }

    pub(crate) fn compatibility(reason: impl Into<String>) -> Self {
        Self::Compatibility {
            reason: reason.into(),
        }
    }

    #[cfg(unix)]
    pub(crate) fn uses_keyboard_enhancement(&self) -> bool {
        matches!(self, Self::Kitty)
    }

    pub(crate) fn shift_enter_supported(&self) -> bool {
        !matches!(self, Self::Compatibility { .. })
    }

    pub(crate) fn keyboard_limitation(&self) -> Option<&str> {
        match self {
            Self::Compatibility { reason } => Some(reason),
            Self::Kitty => None,
            #[cfg(windows)]
            Self::NativeWindows => None,
        }
    }
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        // UiState::default is also used by deterministic renderer fixtures.
        // The real runner replaces this before the first frame is drawn.
        Self::kitty()
    }
}
