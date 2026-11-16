# PopSub code style

These rules apply to every Rust file in the workspace, tests and examples included.

## Docstrings

- **One module docstring per file.** Every `.rs` file starts with `//!` lines: a one-sentence summary of what the file holds, then, if useful, a short paragraph on how it fits with the rest of the crate.
- **One docstring per function and per type.** Every `fn` (public, private, trait methods, test functions), `struct`, `enum`, `trait`, `type` alias, `const` and `static` has a `///` docstring directly above it (above any attributes such as `#[derive]` or `#[tokio::test]`). Struct fields and enum variants get a short `///` line when their meaning isn't obvious from the name.
- **Google style.** Sections are written as plain labels, in this order, each followed by entries indented four spaces:

```rust
/// Publishes a message to every current subscriber of its topic.
///
/// QoS 1 messages are also tracked until a subscriber acknowledges them.
///
/// Args:
///     msg: The message to deliver. Its timestamp is overwritten.
///
/// Returns:
///     The number of subscribers the message was sent to.
///
/// Raises:
///     PopSubError::Persistence: When the message can't be stored.
pub fn publish(&mut self, msg: Message) -> Result<usize> {
```

- `Args:` lists every parameter except `self`, in order. Omit the section when there are none.
- `Returns:` describes the value. Omit it for `()`. For `Result<T>`, describe the `Ok` value here.
- `Raises:` lists each error the function can return, as `ErrorType::Variant: when`. Use it for every function returning `Result`. If the function can panic, say when in the description paragraph.
- Code samples go under a plain `Example:` label in a fenced block, so doc tests still run. Don't use rustdoc headings such as `# Arguments`, `# Returns`, `# Errors` or `# Example`.
- Test functions get a one-sentence docstring saying what behaviour they check. No sections.
- The summary line says what the item does or is, specifically. Not "Handles stuff for X" or a restatement of the name.

## Comments

- **No inline comments.** No `//` comments anywhere, including tests and examples. If a comment explains something worth keeping (a non-obvious reason, an invariant, a lock order), move that explanation into the enclosing item's docstring. Otherwise delete it.
- No decorative separators (`// =====`, `// -----`), numbered step comments (`// 1. Login`), or given/when/then markers in tests.
- `TODO` notes become issues, not comments.

## Wording

- Plain, specific English. No emoji. No stock phrases: "This ensures", "robust", "comprehensive", "seamless(ly)", "leverage", "production-ready", "for backward compatibility" (unless something old really depends on it).
- No em dashes or en dashes. Use commas, full stops or parentheses.
- Say what the code does and why, not that it is good.
