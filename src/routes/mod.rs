pub mod admin;
pub mod public;

/// Empty or whitespace-only query values count as absent (`?resourceId=&resourceUrl=/x`).
pub(crate) fn nonempty(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.trim().is_empty())
}
