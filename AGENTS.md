# Agent rules

- Do not commit NJ Transit credentials, tokens, captured responses, or feed bytes.
- Do not commit NJ Transit API PDFs or paste their text into the repo. Describe behavior in our own words.
- Respect NJ Transit rate limits. Rail `getToken` shares a budget of 10 calls per America/New_York day with `isValidToken`. This proxy never calls `isValidToken`. Do not raise `TokenPolicy::daily_cap` above 10.
- Agents never merge, tag, or publish this repository.
- Document every public and private item. Crate docs include a module map and an example. Functions document `# Arguments`, `# Returns`, `# Errors`, `# Panics`, and `# Examples`. Examples must compile. Examples that would contact NJ Transit are `no_run` and use placeholder credentials such as `username` and `password`.
- Enforce `missing_docs`, `clippy::missing_docs_in_private_items`, `clippy::missing_errors_doc`, `clippy::missing_panics_doc`, `rustdoc::broken_intra_doc_links`, and `rustdoc::private_intra_doc_links`. `clippy.toml` sets `check-private-items` so the error and panic doc lints cover private functions.
- Before opening a PR, from a clean tree, run:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo test --workspace --locked`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --bins --no-deps --document-private-items --locked`

  The workspace needs `--workspace` (there is no default package) and `--bins` so the binary crate is documented too.
