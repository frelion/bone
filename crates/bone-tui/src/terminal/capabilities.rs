/// The transport BONE uses to distinguish a modified Enter key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KeyboardProtocol {
    #[cfg(windows)]
    /// Native Windows console events carry modifiers without a VT extension.
    NativeWindows,
    /// Kitty's progressive keyboard protocol, transported as CSI-u sequences.
    Kitty,
    /// The terminal cannot prove that Enter and Shift+Enter are distinct.
    Compatibility { reason: String },
}

/// Capabilities negotiated for this terminal session.
///
/// This value describes runtime I/O only. It never changes terminal, shell, or
/// multiplexer configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalCapabilities {
    keyboard: KeyboardProtocol,
}

impl TerminalCapabilities {
    #[cfg(windows)]
    pub(crate) fn native_windows() -> Self {
        Self {
            keyboard: KeyboardProtocol::NativeWindows,
        }
    }

    pub(crate) fn kitty() -> Self {
        Self {
            keyboard: KeyboardProtocol::Kitty,
        }
    }

    pub(crate) fn compatibility(reason: impl Into<String>) -> Self {
        Self {
            keyboard: KeyboardProtocol::Compatibility {
                reason: reason.into(),
            },
        }
    }

    pub(crate) fn uses_keyboard_enhancement(&self) -> bool {
        matches!(self.keyboard, KeyboardProtocol::Kitty)
    }

    pub(crate) fn shift_enter_supported(&self) -> bool {
        !matches!(self.keyboard, KeyboardProtocol::Compatibility { .. })
    }

    pub(crate) fn keyboard_limitation(&self) -> Option<&str> {
        match &self.keyboard {
            KeyboardProtocol::Compatibility { reason } => Some(reason),
            KeyboardProtocol::Kitty => None,
            #[cfg(windows)]
            KeyboardProtocol::NativeWindows => None,
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
