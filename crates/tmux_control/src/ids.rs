use std::fmt;
use std::str::FromStr;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid tmux id {0:?}")]
pub struct IdParseError(String);

macro_rules! tmux_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.strip_prefix($prefix)
                    .and_then(|digits| digits.parse().ok())
                    .map(Self)
                    .ok_or_else(|| IdParseError(s.to_owned()))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

tmux_id!(PaneId, "%");
tmux_id!(WindowId, "@");
tmux_id!(SessionId, "$");

#[cfg(test)]
#[path = "ids_tests.rs"]
mod tests;
