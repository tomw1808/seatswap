//! Whether a model keeps its earlier thinking across a switch (spec §2.11).
//!
//! Table as of 14 November 2026: thinking from Claude Fable 5.2, Mythos 5.2,
//! Sonnet 5.5 and Haiku 5.5 works only in the organization that produced it, so
//! a switch makes a running session continue without it. The Opus 5.5 and
//! Fable 5.1 models are not on that list and keep it. A model id the table does
//! not know counts as tied: the careful assumption.

/// Models whose thinking is tied to the account, compared as whole ids.
const TIED: [&str; 4] = [
    "claude-fable-5-2",
    "claude-mythos-5-2",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
];

/// Models whose thinking survives a switch, compared as whole ids.
const NOT_TIED: [&str; 7] = [
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-fable-5-1",
    "claude-mythos-5-1",
    "claude-fable-5",
    "claude-mythos-5",
    "claude-sonnet-5",
];

/// Older model families whose thinking survives a switch, compared as prefixes.
const NOT_TIED_PREFIXES: [&str; 4] = [
    "claude-opus-4",
    "claude-sonnet-4",
    "claude-haiku-4",
    "claude-3",
];

/// The id with one trailing `[...]` group removed, such as the `[1m]` Claude
/// Code appends in `claude-opus-5-5[1m]`; nothing else is changed. A `[` without
/// a closing `]`, or an id without a suffix, is returned as it is.
fn without_suffix(model_id: &str) -> &str {
    let Some(rest) = model_id.strip_suffix(']') else {
        return model_id;
    };
    match rest.rfind('[') {
        Some(open) => &rest[..open],
        None => model_id,
    }
}

/// Whether a session on this model loses its earlier thinking on another account
/// (spec §2.11): the table above, exact ids as whole ids and older families by
/// prefix; any other id is tied. One trailing `[...]` suffix, such as Claude
/// Code's `[1m]` in `claude-opus-5-5[1m]`, is stripped before the comparison;
/// nothing else is changed.
///
/// Table as of 14 November 2026. Tied: `claude-fable-5-2`, `claude-mythos-5-2`,
/// `claude-sonnet-5-5`, `claude-haiku-5-5`. Not tied: `claude-opus-5-5`,
/// `claude-opus-5`, `claude-fable-5-1`, `claude-mythos-5-1`, `claude-fable-5`,
/// `claude-mythos-5`, `claude-sonnet-5`, and every id that starts with
/// `claude-opus-4`, `claude-sonnet-4`, `claude-haiku-4` or `claude-3`.
pub fn thinking_tied(model_id: &str) -> bool {
    let model_id = without_suffix(model_id);
    if TIED.contains(&model_id) {
        return true;
    }
    if NOT_TIED.contains(&model_id) {
        return false;
    }
    if NOT_TIED_PREFIXES
        .iter()
        .any(|prefix| model_id.starts_with(prefix))
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_tied_table() {
        // The plan's ids as literals, so a wrong constant is caught here.
        assert!(thinking_tied("claude-fable-5-2"));
        assert!(thinking_tied("claude-sonnet-5-5"));
        assert!(!thinking_tied("claude-opus-5-5"));
        assert!(!thinking_tied("claude-opus-4-8"));
        assert!(!thinking_tied("claude-fable-5-1"));
        assert!(thinking_tied("something-new-9"));
        for id in TIED {
            assert!(thinking_tied(id), "{id}");
        }
        for id in NOT_TIED {
            assert!(!thinking_tied(id), "{id}");
        }
        for id in [
            "claude-opus-4-8",
            "claude-opus-4-1",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "claude-3-7-sonnet",
        ] {
            assert!(!thinking_tied(id), "{id}");
        }
        // Claude Code appends a context-window suffix such as `[1m]`.
        assert!(thinking_tied("claude-fable-5-2[1m]"), "suffixed tied id");
        assert!(thinking_tied("claude-sonnet-5-5[1m]"), "suffixed tied id");
        assert!(!thinking_tied("claude-opus-5-5[1m]"), "suffixed untied id");
        assert!(!thinking_tied("claude-fable-5-1[1m]"), "suffixed untied id");
        assert!(
            !thinking_tied("claude-opus-4-8[1m]"),
            "suffixed family prefix"
        );
        assert!(thinking_tied("something-new-9[1m]"), "suffixed unknown id");
        // One suffix only: what is left still has to be a known id.
        assert!(thinking_tied("claude-opus-5-5[1m][2m]"), "two suffixes");
    }
}
