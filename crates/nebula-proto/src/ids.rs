//! Time-sortable identifiers (ULIDs), serialized as 26-character strings.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Ulid);

        impl $name {
            /// A new identifier for the current time.
            #[must_use]
            pub fn new() -> Self {
                Self(Ulid::generate())
            }

            /// Wraps an existing ULID (for tests and replay).
            #[must_use]
            pub const fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }

            /// The underlying ULID.
            #[must_use]
            pub const fn ulid(self) -> Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self)
            }
        }
    };
}

ulid_id!(
    /// Identifies one end-to-end operation (a chat turn, later a task step) across every log
    /// event and model call it causes.
    TraceId
);
ulid_id!(
    /// Identifies one span inside a trace.
    SpanId
);
ulid_id!(
    /// Identifies one streaming chat started with `chat.start`.
    ChatId
);
